package game

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// raise plants one runestone for a player standing next to its anchor and returns it.
func (h *structureHarness) raise(p *Player, anchor [3]int32) *structure {
	h.t.Helper()

	h.give(p, 0, ItemRunestone, 1)
	if _, reason, err := p.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		h.t.Fatalf("raising a runestone at %v: %v (%s)", anchor, err, reason)
	}
	for _, standing := range h.structures() {
		if standing.kind == vnet.StructureKindRunestone && standing.anchor == anchor {
			return standing
		}
	}
	h.t.Fatalf("no runestone stands at %v after a placement that succeeded", anchor)
	return nil
}

func (h *structureHarness) ward(col world.Column) (identity.PlayerID, bool) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.sim.wardOf(col)
}

func mineRequest(pos [3]int32, tick uint32) protocol.MineRequest {
	return protocol.MineRequest{Pos: pos, HasPos: true, Active: true, ClientTick: tick}
}

// The claim is a 3x3 Chebyshev square of columns and reaches every Y.
func TestARunestoneWardsTheNineColumnsAroundItsOwn(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	stone := h.raise(player, [3]int32{0, 63, 0})

	centre := stone.chunk.Column()
	for dx := int32(-WardChunkRadius); dx <= WardChunkRadius; dx++ {
		for dz := int32(-WardChunkRadius); dz <= WardChunkRadius; dz++ {
			col := world.Column{CX: centre.CX + dx, CZ: centre.CZ + dz}
			owner, warded := h.ward(col)
			if !warded {
				t.Errorf("column %+v is not warded, and it is %d,%d from the stone's own", col, dx, dz)
				continue
			}
			if owner != player.playerID {
				t.Errorf("column %+v is warded by %s, want the player who raised the stone", col, owner.Short())
			}
		}
	}

	for _, outside := range []world.Column{
		{CX: centre.CX + WardChunkRadius + 1, CZ: centre.CZ},
		{CX: centre.CX, CZ: centre.CZ - WardChunkRadius - 1},
	} {
		if _, warded := h.ward(outside); warded {
			t.Errorf("column %+v is warded, and it is outside the Chebyshev radius of %d", outside, WardChunkRadius)
		}
	}

	// Every height, because a claim somebody can tunnel under is not one. The column is
	// the whole of the key, so this is a statement about the *type* as much as about the
	// value: there is no cy for a caller to get wrong.
	for _, y := range []int64{-64, 0, 63, 200} {
		if !h.sim.wardedAgainstLockedForTest([3]int64{4, y, 4}, identity.PlayerID{}) {
			t.Errorf("the voxel at y=%d inside the warded column is not warded", y)
		}
	}
}

func TestARunestoneNeedsThreeCellsOfHeadroom(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.give(player, 0, ItemRunestone, 1)
	h.world.set([3]int64{0, 66, 0}, world.Stone)
	if _, reason, err := player.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth)); err == nil {
		t.Fatal("a runestone was raised through its third headroom cell")
	} else if reason != vnet.RefusalReasonSpaceBlocked {
		t.Errorf("reason = %s, want SpaceBlocked", reason)
	}
}

func (s *Sim) wardedAgainstLockedForTest(voxel [3]int64, actor identity.PlayerID) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.wardedAgainstLocked(voxel, actor)
}

// Lower structure ids win overlaps, independent of map iteration order.
func TestTheEarlierRunestoneKeepsTheOverlap(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	first, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	// Two chunk columns apart, so the two 3x3 squares share the column between them.
	second, _ := h.join(2, [3]float32{float32(2*ChunkSizeBlocks) + 0.5, 64, 0.5})

	earlier := h.raise(first, [3]int32{0, 63, 0})
	later := h.raise(second, [3]int32{2 * ChunkSizeBlocks, 63, 0})
	if earlier.structureID >= later.structureID {
		t.Fatalf("structure ids %d and %d are not in placement order", earlier.structureID, later.structureID)
	}

	shared := world.Column{CX: earlier.chunk.Column().CX + 1, CZ: earlier.chunk.Column().CZ}
	if shared != (world.Column{CX: later.chunk.Column().CX - 1, CZ: later.chunk.Column().CZ}) {
		t.Fatalf("the two stones do not overlap on column %+v; the fixture is wrong, not the rule", shared)
	}

	owner, warded := h.ward(shared)
	if !warded {
		t.Fatal("the shared column is not warded at all")
	}
	if owner != first.playerID {
		t.Errorf("the shared column is warded by the later stone's owner; the earlier one wins an overlap")
	}
}

// The cap belongs to an identity and shares the tent's refusal reason.
func TestAThirdRunestoneIsRefused(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	for i := range MaxRunestonesPerPlayer {
		h.raise(player, [3]int32{int32(2 * i), 63, 0})
	}

	h.give(player, 0, ItemRunestone, 1)
	_, reason, err := player.PlaceStructure(placeRequest(0, [3]int32{0, 63, 2}, vnet.FacingNorth))
	if err == nil {
		t.Fatalf("a %dth runestone was allowed", MaxRunestonesPerPlayer+1)
	}
	if reason != vnet.RefusalReasonTentAlreadyPlaced {
		t.Errorf("reason = %s, want TentAlreadyPlaced — the shared 'you already have one' answer", reason)
	}
	if standing := len(h.structures()); standing != MaxRunestonesPerPlayer {
		t.Errorf("%d structures stand, want %d — the refused placement inserted one", standing, MaxRunestonesPerPlayer)
	}

	// The refusal is a rule about raising one, so a second player is unaffected by the
	// first player's budget.
	otherX := int32(4 * ChunkSizeBlocks)
	other, _ := h.join(2, [3]float32{float32(otherX) + 0.5, 64, 0.5})
	h.raise(other, [3]int32{otherX, 63, 0})
}

// Placement, editing and mining share one owner exemption.
func TestAWardRefusesEveryoneButItsOwner(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.raise(owner, [3]int32{0, 63, 0})

	// Inside the stone's own column, and within reach of both players standing here.
	target := [3]int32{2, 63, 2}
	intruder, _ := h.join(2, [3]float32{2.5, 64, 2.5})

	t.Run("placement", func(t *testing.T) {
		h.give(intruder, 0, ItemCampfire, 1)
		_, reason, err := intruder.PlaceStructure(placeRequest(0, target, vnet.FacingNorth))
		if err == nil {
			t.Fatal("a campfire went up on warded ground")
		}
		if reason != vnet.RefusalReasonWarded {
			t.Errorf("reason = %s, want Warded", reason)
		}
	})

	t.Run("edit", func(t *testing.T) {
		h.give(intruder, 1, ItemStone, 1)
		_, err := intruder.Edit(context.Background(), protocol.BlockEditRequest{
			Action: vnet.EditActionPlace, Pos: [3]int32{2, 64, 2}, HasPos: true, Slot: 1,
		})
		if !errors.Is(err, ErrWarded) {
			t.Errorf("Edit inside a ward returned %v, want ErrWarded", err)
		}
	})

	t.Run("mine", func(t *testing.T) {
		if err := intruder.Mine(mineRequest(target, 1), true); !errors.Is(err, ErrWarded) {
			t.Errorf("Mine inside a ward returned %v, want ErrWarded", err)
		}
	})

	t.Run("the owner is exempt", func(t *testing.T) {
		h.give(owner, 0, ItemCampfire, 1)
		if _, reason, err := owner.PlaceStructure(placeRequest(0, target, vnet.FacingNorth)); err != nil {
			t.Fatalf("the owner could not build on their own ground: %v (%s)", err, reason)
		}

		h.give(owner, 1, ItemStone, 1)
		if _, err := owner.Edit(context.Background(), protocol.BlockEditRequest{
			Action: vnet.EditActionPlace, Pos: [3]int32{1, 64, 1}, HasPos: true, Slot: 1,
		}); err != nil {
			t.Errorf("the owner could not edit their own ground: %v", err)
		}
		if err := owner.Mine(mineRequest([3]int32{1, 63, 1}, 1), true); err != nil {
			t.Errorf("the owner could not mine their own ground: %v", err)
		}
	})

	t.Run("unclaimed ground is nobody's", func(t *testing.T) {
		// Far enough out to be in a column no stone reaches, and the player is moved
		// there so the reach check is not what answers.
		far := [3]int32{4 * ChunkSizeBlocks, 63, 0}
		outsider, _ := h.join(3, [3]float32{float32(4*ChunkSizeBlocks) + 0.5, 64, 0.5})
		if err := outsider.Mine(mineRequest(far, 1), true); err != nil {
			t.Errorf("mining unwarded ground was refused: %v", err)
		}
	})
}

// Even a structure's owner cannot remove it from another identity's ward.
func TestAStructureInsideAnothersWardCannotBeRemoved(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	builder, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.give(builder, 0, ItemCampfire, 1)
	if _, _, err := builder.PlaceStructure(placeRequest(0, [3]int32{2, 63, 2}, vnet.FacingNorth)); err != nil {
		t.Fatalf("placing the fire: %v", err)
	}
	var fire *structure
	for _, standing := range h.structures() {
		if standing.kind == vnet.StructureKindCampfire {
			fire = standing
		}
	}

	claimant, _ := h.join(2, [3]float32{0.5, 64, 0.5})
	h.raise(claimant, [3]int32{0, 63, 0})

	if err := builder.RemoveStructure(protocol.RemoveStructureRequest{StructureID: fire.structureID}); err == nil {
		t.Fatal("a fire inside another player's ward was taken down by its owner")
	}
	if standing := len(h.structures()); standing != 2 {
		t.Errorf("%d structures stand, want 2 — the refused removal took one down", standing)
	}
}

// The anchor is in unclaimed column 2, while the tent's western cells cross into
// claimed column 1. The whole structure occupies claimed ground, not only its anchor.
func TestAWardRefusesAPlacementWhoseFootprintCrossesItsBoundary(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	claimant, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.raise(claimant, [3]int32{0, 63, 0})

	anchorX := int32(2 * ChunkSizeBlocks)
	builder, _ := h.join(2, [3]float32{float32(anchorX) + 0.5, 64, 0.5})
	h.give(builder, 0, ItemTent, 1)
	_, reason, err := builder.PlaceStructure(placeRequest(0, [3]int32{anchorX, 63, 0}, vnet.FacingNorth))
	if err == nil {
		t.Fatal("a tent crossed from an unclaimed anchor into another player's ward")
	}
	if reason != vnet.RefusalReasonWarded {
		t.Errorf("reason = %s, want Warded", reason)
	}
}

// A structure built before its neighbour raised a stone is protected across the same
// boundary when its owner later tries to remove it.
func TestAWardRefusesRemovalWhenAnyFootprintCellCrossesItsBoundary(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	anchorX := int32(2 * ChunkSizeBlocks)
	builder, _ := h.join(1, [3]float32{float32(anchorX) + 0.5, 64, 0.5})
	tent := h.plantTent(builder, [3]int32{anchorX, 63, 0})

	claimant, _ := h.join(2, [3]float32{0.5, 64, 0.5})
	h.raise(claimant, [3]int32{0, 63, 0})
	if err := builder.RemoveStructure(protocol.RemoveStructureRequest{StructureID: tent.structureID}); err == nil {
		t.Fatal("a tent crossing into another player's ward was removed")
	}
	if standing := len(h.structures()); standing != 2 {
		t.Errorf("%d structures stand, want the tent and runestone", standing)
	}
}

// Generation happens before the authoritative write guard. A ward raised during that
// wait must be re-read by the guard, rather than letting the edit use its earlier answer.
func TestAWardRaisedWhileAnEditGeneratesRefusesTheWrite(t *testing.T) {
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
	player, err := sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 200, 0.5}, testAppearance(), nil, func([]byte) bool { return true })
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

	sim.mu.Lock()
	stoneID := sim.mintEntityID()
	sim.structures[stoneID] = &structure{
		structureID: stoneID,
		kind:        vnet.StructureKindRunestone,
		anchor:      [3]int32{0, 199, 0},
		facing:      vnet.FacingNorth,
		owner:       testPlayerID(2),
		chunk:       world.ChunkOf(0, 199, 0),
	}
	sim.rebuildWardsLocked()
	sim.mu.Unlock()
	close(editor.finishGeneration)

	select {
	case editErr := <-result:
		if !errors.Is(editErr, ErrWarded) {
			t.Fatalf("Edit returned %v, want ErrWarded", editErr)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for the guarded edit")
	}
	if got := player.InventoryState().Stacks[0].Count; got != 1 {
		t.Errorf("the refused edit spent the stack, leaving %d", got)
	}
}

// Taking a stone down drops only its claim, preserving overlaps.
func TestRemovingARunestoneDropsItsWard(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	stone := h.raise(player, [3]int32{0, 63, 0})

	if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: stone.structureID}); err != nil {
		t.Fatalf("RemoveStructure: %v", err)
	}
	if _, warded := h.ward(stone.chunk.Column()); warded {
		t.Error("the ward outlived the stone that cast it")
	}

	// And a neighbour's overlapping claim survives its neighbour's removal, which is the
	// case a per-column deletion would have got wrong.
	second, _ := h.join(2, [3]float32{float32(2*ChunkSizeBlocks) + 0.5, 64, 0.5})
	kept := h.raise(second, [3]int32{2 * ChunkSizeBlocks, 63, 0})
	third := h.raise(player, [3]int32{0, 63, 0})
	_ = third

	shared := world.Column{CX: kept.chunk.Column().CX - 1, CZ: kept.chunk.Column().CZ}
	if _, warded := h.ward(shared); !warded {
		t.Fatal("the fixture's two stones do not overlap")
	}
	if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: third.structureID}); err != nil {
		t.Fatalf("removing the overlapping stone: %v", err)
	}
	owner, warded := h.ward(shared)
	if !warded || owner != second.playerID {
		t.Error("removing one stone dropped a column the other stone still claims")
	}
}

// Collapse removes the ward with the runestone.
func TestCollapsingARunestoneDropsItsWard(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	stone := h.raise(player, [3]int32{0, 63, 0})

	if _, err := player.breakMined(context.Background(), stone.anchor, world.Stone); err != nil {
		t.Fatalf("breaking the ground under the stone: %v", err)
	}
	if standing := len(h.structures()); standing != 0 {
		t.Fatalf("%d structures stand, want 0 — the stone did not come down", standing)
	}
	if _, warded := h.ward(stone.chunk.Column()); warded {
		t.Error("the ward outlived the stone the break brought down")
	}
}

// Restore derives wards from structures rather than reading a second persisted copy.
func TestARestoredRunestoneWardsItsGroundAgain(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner := testPlayerID(1)
	anchor := [3]int32{0, 63, 0}

	if err := h.sim.RestoreStructures([]Structure{
		{Kind: vnet.StructureKindRunestone, Anchor: anchor, Facing: vnet.FacingNorth, Owner: owner},
	}); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}

	col := world.ChunkOf(int64(anchor[0]), int64(anchor[1]), int64(anchor[2])).Column()
	restored, warded := h.ward(col)
	if !warded {
		t.Fatal("a restored runestone wards nothing")
	}
	if restored != owner {
		t.Errorf("the restored ward is owned by %s, want the stored owner", restored.Short())
	}
}

// The placement cap does not make an older, larger stored camp unloadable.
func TestRestoreKeepsMoreRunestonesThanTheCapAllows(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	owner := testPlayerID(1)

	stored := make([]Structure, 0, MaxRunestonesPerPlayer+1)
	for i := range MaxRunestonesPerPlayer + 1 {
		stored = append(stored, Structure{
			Kind:   vnet.StructureKindRunestone,
			Anchor: [3]int32{int32(2 * i), 63, 0},
			Facing: vnet.FacingNorth,
			Owner:  owner,
		})
	}
	if err := h.sim.RestoreStructures(stored); err != nil {
		t.Fatalf("RestoreStructures refused %d stones for one owner: %v", len(stored), err)
	}
	if standing := len(h.structures()); standing != len(stored) {
		t.Errorf("%d structures stand, want %d", standing, len(stored))
	}
}

// ChunkSizeBlocks is the chunk edge as the int32 an anchor is written in.
const ChunkSizeBlocks = int32(world.ChunkSize)
