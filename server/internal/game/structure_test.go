package game

import (
	"context"
	"io"
	"log/slog"
	"math"
	"sync"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

// structureWorld is a flat world that can also be edited, which is what makes it both
// the Terrain a footprint is checked against and the Editor a break goes through.
//
// One type rather than two, because the collapse rule is the one place those two roles
// meet: the block a break writes has to be the block the next footprint check reads, and
// a fixture with two copies of the world would let a test pass while they disagreed.
type structureWorld struct {
	mu        sync.Mutex
	groundTop int64
	blocks    map[[3]int64]world.Block
	absent    map[[3]int64]bool
}

func newStructureWorld(groundTop int64) *structureWorld {
	return &structureWorld{
		groundTop: groundTop,
		blocks:    make(map[[3]int64]world.Block),
		absent:    make(map[[3]int64]bool),
	}
}

func (w *structureWorld) Block(x, y, z int64) (world.Block, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.blockAtLocked([3]int64{x, y, z})
}
func (w *structureWorld) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w *structureWorld) blockAtLocked(voxel [3]int64) (world.Block, bool) {
	if w.absent[voxel] {
		return world.Air, false
	}
	if block, edited := w.blocks[voxel]; edited {
		return block, true
	}
	if voxel[1] <= w.groundTop {
		return world.Stone, true
	}
	return world.Air, true
}

func (w *structureWorld) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

func (w *structureWorld) ApplyGuarded(_ context.Context, x, y, z int64, block world.Block, guard func() error, allow func(world.Block) error) error {
	if guard != nil {
		if err := guard(); err != nil {
			return err
		}
	}

	w.mu.Lock()
	defer w.mu.Unlock()
	voxel := [3]int64{x, y, z}
	if allow != nil {
		current, _ := w.blockAtLocked(voxel)
		if err := allow(current); err != nil {
			return err
		}
	}
	w.blocks[voxel] = block
	return nil
}

// set puts a block in the world without going through an edit, so a test can wall a
// footprint in or carve a hole under one before anybody asks about it.
func (w *structureWorld) set(voxel [3]int64, block world.Block) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.blocks[voxel] = block
}

// hide marks a voxel as terrain the server has not composed yet, which is the one answer
// a footprint check has to treat as neither solid nor clear.
func (w *structureWorld) hide(voxel [3]int64) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.absent[voxel] = true
}

// structureHarness drives a simulation one tick at a time against a world that can be
// both read and edited.
type structureHarness struct {
	t     *testing.T
	sim   *Sim
	world *structureWorld
	tick  uint64
}

func newStructureHarness(t *testing.T) *structureHarness {
	t.Helper()
	return newStructureHarnessAt(t, 8)
}

func newStructureHarnessAt(t *testing.T, viewDistance uint8) *structureHarness {
	t.Helper()

	fixture := newStructureWorld(63)
	sim, err := NewSim(DefaultTickRate, viewDistance, testWorldSeed, fixture, fixture, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	return &structureHarness{t: t, sim: sim, world: fixture}
}

func (h *structureHarness) join(entityID uint64, pos [3]float32) (*Player, *dropSink) {
	h.t.Helper()

	out := &dropSink{}
	player, err := h.sim.Join(entityID, testPlayerID(entityID), testCharacterName, pos, testAppearance(), nil, out.deliver)
	if err != nil {
		h.t.Fatalf("Join: %v", err)
	}
	return player, out
}

func (h *structureHarness) step() {
	h.tick++
	h.sim.Step(h.tick)
}

func (h *structureHarness) advance(n int) {
	h.t.Helper()
	for range n {
		h.step()
	}
}

// give puts an item straight into a slot. Structures are not craftable in this issue, so
// this is how a test gets one — the same shortcut the existing drop tests take.
func (h *structureHarness) give(p *Player, slot uint8, item ItemID, count uint16) {
	h.t.Helper()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	p.inventory.slots[slot] = stackOf(item, count)
}

// place is the request a client sends, with the fields a test rarely varies filled in.
func placeRequest(slot uint8, anchor [3]int32, facing vnet.Facing) protocol.PlaceStructureRequest {
	return protocol.PlaceStructureRequest{Slot: slot, Anchor: anchor, HasAnchor: true, Facing: facing}
}

// structures is every structure standing, in identity order.
func (h *structureHarness) structures() []*structure {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.sim.sortedStructuresLocked()
}

// only is the single structure standing, and a failure when there is any other number.
func (h *structureHarness) only() *structure {
	h.t.Helper()

	standing := h.structures()
	if len(standing) != 1 {
		h.t.Fatalf("%d structures stand in the world, want exactly 1", len(standing))
	}
	return standing[0]
}

// plantTent puts a tent at the anchor through the authoritative path and returns it.
func (h *structureHarness) plantTent(p *Player, anchor [3]int32) *structure {
	h.t.Helper()

	h.give(p, 0, ItemTent, 1)
	if _, _, err := p.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		h.t.Fatalf("planting a tent at %v: %v", anchor, err)
	}
	return h.only()
}

// plantCampfire puts a campfire at the anchor through the authoritative path.
//
// Through PlaceStructure, unlike the same-named helper on the spawn director's harness,
// which writes a registry entry directly because the structure did not exist when it was
// written. Here the placement *is* the thing under test, so the fire has to arrive the way
// a player's would.
func (h *structureHarness) plantCampfire(p *Player, slot uint8, anchor [3]int32) *structure {
	h.t.Helper()

	h.give(p, slot, ItemCampfire, 1)
	if _, _, err := p.PlaceStructure(placeRequest(slot, anchor, vnet.FacingNorth)); err != nil {
		h.t.Fatalf("planting a campfire at %v: %v", anchor, err)
	}
	return h.only()
}

// ---------------------------------------------------------------------------
// The vocabulary the batch pinned
// ---------------------------------------------------------------------------

// The item ids the client mirrors to draw a held shape, and the registry entries that say
// a structure is not a block.
//
// Pinned rather than derived, because iota renumbers everything after an insertion and
// the failure mode is a client drawing a forge in a hand holding a tent.
func TestStructureItemsCarryThePinnedIdsAndPlaceNoBlock(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		item ItemID
		want ItemID
	}{
		{"Forge", ItemForge, 8},
		{"Tent", ItemTent, 9},
		{"Campfire", ItemCampfire, 12},
	} {
		if tc.item != tc.want {
			t.Errorf("%s = %d, want %d", tc.name, tc.item, tc.want)
		}

		definition, registered := itemByID(tc.item)
		if !registered {
			t.Errorf("%s is not registered", tc.name)
			continue
		}
		if definition.maxStack != 1 {
			t.Errorf("%s stacks to %d, want 1", tc.name, definition.maxStack)
		}
		if definition.maxDurability != 0 {
			t.Errorf("%s has durability %d, want none", tc.name, definition.maxDurability)
		}
		if block, placeable := blockPlacedBy(tc.item); placeable || block != world.Air {
			t.Errorf("%s places block %d (placeable %v), want no voxel at all", tc.name, block, placeable)
		}
	}
}

// The footprint rotates with the facing, and the forge is where that is visible: its
// hearth is one step along the direction the client quantized its camera to.
//
// The compass is the movement integrator's — North is -Z and +X is East — so this is also
// the test that fails if somebody reads a facing as a screen direction.
func TestTheForgeHearthSitsAlongTheFacing(t *testing.T) {
	t.Parallel()

	anchor := [3]int64{4, 63, 7}
	for _, tc := range []struct {
		facing vnet.Facing
		hearth [3]int64
	}{
		{vnet.FacingNorth, [3]int64{4, 63, 6}},
		{vnet.FacingEast, [3]int64{5, 63, 7}},
		{vnet.FacingSouth, [3]int64{4, 63, 8}},
		{vnet.FacingWest, [3]int64{3, 63, 7}},
	} {
		cells, headroom, ok := footprintOf(vnet.StructureKindForge, tc.facing, anchor)
		if !ok {
			t.Fatalf("%s: the forge has no footprint", tc.facing)
		}
		if headroom != forgeHeadroom {
			t.Errorf("%s: forge headroom %d, want %d", tc.facing, headroom, forgeHeadroom)
		}
		want := [][3]int64{anchor, tc.hearth}
		if len(cells) != len(want) || cells[0] != want[0] || cells[1] != want[1] {
			t.Errorf("%s: footprint %v, want %v", tc.facing, cells, want)
		}
	}
}

// A tent's nine cells are symmetric, so every facing describes the same ground. Asserted
// rather than assumed: the rotation runs for a tent exactly as it does for a forge, and a
// sign error in it would show up here as a footprint that moved.
func TestTheTentFootprintIsTheSameNineCellsWhicheverWayItFaces(t *testing.T) {
	t.Parallel()

	anchor := [3]int64{-2, 63, 5}
	north, headroom, ok := footprintOf(vnet.StructureKindTent, vnet.FacingNorth, anchor)
	if !ok {
		t.Fatal("the tent has no footprint")
	}
	if headroom != tentHeadroom {
		t.Errorf("tent headroom %d, want %d", headroom, tentHeadroom)
	}
	if len(north) != 9 {
		t.Fatalf("the tent rests on %d cells, want 9", len(north))
	}

	for _, facing := range []vnet.Facing{vnet.FacingEast, vnet.FacingSouth, vnet.FacingWest} {
		rotated, _, _ := footprintOf(vnet.StructureKindTent, facing, anchor)
		for _, cell := range north {
			if !containsCell(rotated, cell) {
				t.Errorf("%s: the footprint lost cell %v", facing, cell)
			}
		}
		if len(rotated) != len(north) {
			t.Errorf("%s: the footprint has %d cells, want %d", facing, len(rotated), len(north))
		}
	}
}

// The fire rests on the one cell it stands on, whichever way it faces, and needs one cell
// of air over it.
//
// Asserted rather than assumed, exactly as the tent's symmetry is: the rotation runs for a
// campfire as it does for a forge, and a one-cell footprint that moved under a facing would
// be a fire whose safe ground drifted off the block the player put it on.
func TestTheCampfireRestsOnOneCellWhicheverWayItFaces(t *testing.T) {
	t.Parallel()

	anchor := [3]int64{7, 63, -3}
	for _, facing := range []vnet.Facing{vnet.FacingNorth, vnet.FacingEast, vnet.FacingSouth, vnet.FacingWest} {
		cells, headroom, ok := footprintOf(vnet.StructureKindCampfire, facing, anchor)
		if !ok {
			t.Fatalf("%s: the campfire has no footprint", facing)
		}
		if headroom != campfireHeadroom {
			t.Errorf("%s: campfire headroom %d, want %d", facing, headroom, campfireHeadroom)
		}
		if len(cells) != 1 || cells[0] != anchor {
			t.Errorf("%s: footprint %v, want the anchor %v alone", facing, cells, anchor)
		}
	}
}

func containsCell(cells [][3]int64, want [3]int64) bool {
	for _, cell := range cells {
		if cell == want {
			return true
		}
	}
	return false
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

// A tent and a forge both go up on flat ground, whichever way they face, and the item is
// spent exactly once.
func TestAStructureIsPlantedOnGroundThatHoldsItAtEveryFacing(t *testing.T) {
	t.Parallel()

	for _, kind := range []struct {
		name string
		item ItemID
		want vnet.StructureKind
	}{
		{"tent", ItemTent, vnet.StructureKindTent},
		{"forge", ItemForge, vnet.StructureKindForge},
		{"campfire", ItemCampfire, vnet.StructureKindCampfire},
		{"runestone", ItemRunestone, vnet.StructureKindRunestone},
	} {
		for _, facing := range []vnet.Facing{vnet.FacingNorth, vnet.FacingEast, vnet.FacingSouth, vnet.FacingWest} {
			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(player, 3, kind.item, 1)

			state, _, err := player.PlaceStructure(placeRequest(3, [3]int32{0, 63, 0}, facing))
			if err != nil {
				t.Fatalf("%s facing %s: %v", kind.name, facing, err)
			}

			placed := h.only()
			if placed.kind != kind.want {
				t.Errorf("%s facing %s: placed a %s", kind.name, facing, placed.kind)
			}
			if placed.facing != facing {
				t.Errorf("%s: stored facing %s, want %s", kind.name, placed.facing, facing)
			}
			if placed.owner != player.playerID {
				t.Errorf("%s: owner %s, want %s", kind.name, placed.owner.Short(), player.playerID.Short())
			}
			if placed.anchor != [3]int32{0, 63, 0} {
				t.Errorf("%s: anchor %v, want the one the request named", kind.name, placed.anchor)
			}
			if got := heldCount(state, kind.item); got != 0 {
				t.Errorf("%s: the player still holds %d of them, want the placement to have spent it", kind.name, got)
			}
		}
	}
}

// Every reason a footprint does not fit, each asserted to leave the world exactly as it
// was: no structure, and the item still in the slot.
//
// The ground cases and the headroom cases are the same table on purpose — "this thing
// fits here" is one question with two halves, and a refusal that only covered one of them
// would put an anvil in the air or a tent inside a hill.
func TestAFootprintThatDoesNotFitIsRefusedAndCostsNothing(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name   string
		item   ItemID
		break_ func(w *structureWorld)
	}{
		{
			name:   "a corner of the tent's ground is a hole",
			item:   ItemTent,
			break_: func(w *structureWorld) { w.set([3]int64{1, 63, 1}, world.Air) },
		},
		{
			name:   "the tent's ground steps up one block, so the cell at the anchor's height is air",
			item:   ItemTent,
			break_: func(w *structureWorld) { w.set([3]int64{-1, 63, 0}, world.Air) },
		},
		{
			name:   "something stands in the first cell of the tent's headroom",
			item:   ItemTent,
			break_: func(w *structureWorld) { w.set([3]int64{1, 64, 0}, world.Stone) },
		},
		{
			name:   "something stands in the second cell of the tent's headroom",
			item:   ItemTent,
			break_: func(w *structureWorld) { w.set([3]int64{1, 65, 0}, world.Stone) },
		},
		{
			name:   "the tent's ground has not been generated yet",
			item:   ItemTent,
			break_: func(w *structureWorld) { w.hide([3]int64{0, 63, 1}) },
		},
		{
			name:   "the tent's headroom has not been generated yet",
			item:   ItemTent,
			break_: func(w *structureWorld) { w.hide([3]int64{0, 64, 1}) },
		},
		{
			name:   "the forge's hearth has no ground under it",
			item:   ItemForge,
			break_: func(w *structureWorld) { w.set([3]int64{0, 63, -1}, world.Air) },
		},
		{
			name:   "something stands where the forge's anvil goes",
			item:   ItemForge,
			break_: func(w *structureWorld) { w.set([3]int64{0, 64, 0}, world.Stone) },
		},
		{
			// A one-cell footprint has no slope to straddle: the ground it rests on has
			// either stepped away or it has not, and this is that step.
			name:   "the ground under the fire has stepped down, so the anchor's cell is air",
			item:   ItemCampfire,
			break_: func(w *structureWorld) { w.set([3]int64{0, 63, 0}, world.Air) },
		},
		{
			name:   "something stands in the fire's headroom",
			item:   ItemCampfire,
			break_: func(w *structureWorld) { w.set([3]int64{0, 64, 0}, world.Stone) },
		},
		{
			name:   "the ground under the fire has not been generated yet",
			item:   ItemCampfire,
			break_: func(w *structureWorld) { w.hide([3]int64{0, 63, 0}) },
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{2.5, 64, 2.5})
			h.give(player, 0, tc.item, 1)
			tc.break_(h.world)

			_, reason, err := player.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth))
			if err == nil {
				t.Fatal("the placement was accepted")
			}
			// The sweep TestEveryPlacementRefusalNamesItsOwnReason cannot perform: a
			// refusal path added to this table later gets its code checked here without
			// anybody remembering to check it. Unknown is the absent-field value, so a
			// refusal that answers it reaches the player as silence again.
			if reason == vnet.RefusalReasonUnknown {
				t.Errorf("the refusal named no reason: %v", err)
			}
			if standing := len(h.structures()); standing != 0 {
				t.Errorf("%d structures stand in the world, want none", standing)
			}
			if got := heldCount(player.InventoryState(), tc.item); got != 1 {
				t.Errorf("the player holds %d of the item, want the refused placement to have spent nothing", got)
			}
		})
	}
}

// Every refusal that is about the request rather than about the ground. Each leaves the
// world and the pack untouched, and none of them is a protocol error: an out-of-range
// slot and a facing nobody named are things the simulation refuses in silence.
func TestAPlacementRequestTheSimulationRefuses(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		request protocol.PlaceStructureRequest
		prepare func(h *structureHarness, p *Player)
	}{
		{
			name:    "no anchor at all, which is what an absent struct field decodes as",
			request: protocol.PlaceStructureRequest{Slot: 0, Facing: vnet.FacingNorth},
		},
		{
			name:    "a facing the client did not send",
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingUnknown),
		},
		{
			name:    "a facing no member of the enum has",
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.Facing(99)),
		},
		{
			name:    "a slot outside the announced inventory",
			request: placeRequest(protocol.InventorySlots, [3]int32{0, 63, 0}, vnet.FacingNorth),
		},
		{
			name:    "an empty slot",
			request: placeRequest(7, [3]int32{0, 63, 0}, vnet.FacingNorth),
		},
		{
			name:    "a slot holding something that is not a structure",
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) { h.give(p, 0, ItemStone, 8) },
		},
		{
			name:    "an anchor past the reach a block edit obeys",
			request: placeRequest(0, [3]int32{0, 63, 40}, vnet.FacingNorth),
		},
		{
			name:    "a dead player",
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) {
				h.sim.mu.Lock()
				defer h.sim.mu.Unlock()
				p.damageLocked(PlayerMaxHealth)
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(player, 0, ItemTent, 1)
			if tc.prepare != nil {
				tc.prepare(h, player)
			}

			_, reason, err := player.PlaceStructure(tc.request)
			if err == nil {
				t.Fatal("the placement was accepted")
			}
			// Same sweep as the table above, for the same reason.
			if reason == vnet.RefusalReasonUnknown {
				t.Errorf("the refusal named no reason: %v", err)
			}
			if standing := len(h.structures()); standing != 0 {
				t.Errorf("%d structures stand in the world, want none", standing)
			}
		})
	}
}

// One tent to a player, and the rule is about tents and about that player: a second forge
// is fine, and so is somebody else's tent.
func TestOneTentToAPlayerAndNoLimitOnForges(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantTent(player, [3]int32{0, 63, 0})

	h.give(player, 1, ItemTent, 1)
	if _, _, err := player.PlaceStructure(placeRequest(1, [3]int32{3, 63, 0}, vnet.FacingNorth)); err == nil {
		t.Error("a second tent was accepted while the first still stands")
	}
	if got := heldCount(player.InventoryState(), ItemTent); got != 1 {
		t.Errorf("the player holds %d tents, want the refused placement to have spent nothing", got)
	}

	h.give(player, 2, ItemForge, 1)
	if _, _, err := player.PlaceStructure(placeRequest(2, [3]int32{3, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Errorf("a forge beside a standing tent was refused: %v", err)
	}
	h.give(player, 3, ItemForge, 1)
	if _, _, err := player.PlaceStructure(placeRequest(3, [3]int32{-3, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Errorf("a second forge was refused: %v", err)
	}

	other, _ := h.join(2, [3]float32{0.5, 64, 0.5})
	h.give(other, 0, ItemTent, 1)
	if _, _, err := other.PlaceStructure(placeRequest(0, [3]int32{0, 63, 3}, vnet.FacingNorth)); err != nil {
		t.Errorf("another player's tent was refused: %v", err)
	}

	// A tent again once the first has gone, which is what makes the rule "one standing"
	// rather than "one ever".
	first, _ := h.sim.tentOfLocked(player.playerID)
	if first == nil {
		t.Fatal("the player's tent stopped standing on its own")
	}
	if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: first.structureID}); err != nil {
		t.Fatalf("removing the player's own tent: %v", err)
	}
	if _, _, err := player.PlaceStructure(placeRequest(1, [3]int32{3, 63, 3}, vnet.FacingNorth)); err != nil {
		t.Errorf("a replacement tent was refused after the first came down: %v", err)
	}
}

// A camp may have several fires where it may have only one tent, and the two rules are
// deliberately not the same rule. A tent answers "where do I come back to", and two answers
// is a choice nobody made; a fire answers "what ground is safe", and a camp that outgrew one
// patch of it should be able to light another.
//
// The second fire is planted by the same owner, which is the half a per-owner limit would
// have caught, and a third by somebody else, which is the half an anywhere-limit would.
func TestACampMayHaveSeveralFiresWhereItMayHaveOnlyOneTent(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantCampfire(player, 0, [3]int32{0, 63, 0})

	h.give(player, 1, ItemCampfire, 1)
	if _, _, err := player.PlaceStructure(placeRequest(1, [3]int32{2, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Errorf("a second fire from the same owner was refused: %v", err)
	}

	other, _ := h.join(2, [3]float32{0.5, 64, 0.5})
	h.give(other, 0, ItemCampfire, 1)
	if _, _, err := other.PlaceStructure(placeRequest(0, [3]int32{0, 63, 2}, vnet.FacingNorth)); err != nil {
		t.Errorf("somebody else's fire in the same camp was refused: %v", err)
	}

	if standing := len(h.structures()); standing != 3 {
		t.Errorf("%d structures stand in the world, want the three fires that were lit", standing)
	}

	// And the tent's rule is untouched by any of it: a fire is not a tent, and lighting
	// three of them does not buy a second shelter. Both anchors are placed from where the
	// player stands, because reach is checked before the one-tent rule ever runs — a
	// refusal from the wrong check would be this test passing for the wrong reason.
	h.standAt(player, [3]float64{6.5, 64, 0.5})
	h.give(player, 2, ItemTent, 1)
	if _, _, err := player.PlaceStructure(placeRequest(2, [3]int32{6, 63, 0}, vnet.FacingNorth)); err != nil {
		t.Fatalf("planting the tent: %v", err)
	}
	h.give(player, 3, ItemTent, 1)
	if _, _, err := player.PlaceStructure(placeRequest(3, [3]int32{8, 63, 0}, vnet.FacingNorth)); err == nil {
		t.Error("a second tent was accepted beside three fires")
	}
}

// The one gameplay effect a fire has this iteration, asserted at the boundary rather than
// well away from it: the spawn director's predicate answers yes just inside the radius and
// no just outside it, for a fire that arrived through the ordinary placement path.
//
// **This is the loop between the two issues closing.** The director declares
// [CampfireSafeRadius] and [Sim.nearACampfireLocked] and was correct with no fires in the
// world; what is added here is a fire for it to measure from. The predicate is not
// re-implemented — asserting it against a placed structure is the whole point, because a
// second copy of "is one of these within r of here" is a second answer that can disagree.
//
// The distance is purely vertical, so the test states one number rather than a triangle:
// the anchor voxel's centre is at y 63.5 and the body centre sits PlayerHeight/2 above the
// feet, which is the convention every reach rule on this side shares.
func TestAPlacedFireIsTheGroundTheSpawnDirectorKeepsClear(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name     string
		distance float64
		near     bool
	}{
		{"just inside the radius", CampfireSafeRadius - 0.1, true},
		{"just outside the radius", CampfireSafeRadius + 0.1, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.plantCampfire(player, 0, [3]int32{0, 63, 0})

			spot := [3]float64{0.5, 63.5 + tc.distance - PlayerHeight/2, 0.5}

			h.sim.mu.Lock()
			near := h.sim.nearACampfireLocked(spot)
			h.sim.mu.Unlock()

			if near != tc.near {
				t.Errorf("a spot %.1f blocks from the fire reports near=%v, want %v", tc.distance, near, tc.near)
			}
		})
	}
}

// A fire is not a station, and nothing about it was quietly wired into the forge's rule:
// standing beside one buys no forge recipe.
func TestAFireIsNotSomewhereToWork(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantCampfire(player, 0, [3]int32{0, 63, 0})
	h.stockPack(player, recipeTable[vnet.RecipeIDSharpeningStone].ingredients...)

	if _, err := h.craft(player, vnet.RecipeIDSharpeningStone); err == nil {
		t.Error("a forge recipe was crafted at a campfire")
	}
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

// Taking your own structure back: it stops standing, and the item falls through free
// space onto its intact support rather than beginning inside that support. Every kind,
// because they all reach the same removal path and all have to be visible before pickup.
func TestRemovingYourOwnStructureLeavesItsItemOnTheGround(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name         string
		item         ItemID
		blockAbove   bool
		spawnVoxelY  int64
		restingFloor float64
	}{
		{"tent", ItemTent, false, 64, 64},
		{"forge", ItemForge, false, 64, 64},
		{"campfire", ItemCampfire, false, 64, 64},
		{"tent under an overhang", ItemTent, true, 65, 65},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, out := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(player, 0, tc.item, 1)
			if _, _, err := player.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth)); err != nil {
				t.Fatalf("planting the %s: %v", tc.name, err)
			}
			planted := h.only()
			if tc.blockAbove {
				// Terrain may change after placement. Stand within removal reach but out
				// of the edited cell, then put an overhang immediately above the anchor.
				h.sim.mu.Lock()
				player.pos = [3]float64{4, 64, 0.5}
				h.sim.mu.Unlock()
				h.world.set([3]int64{0, 64, 0}, world.Stone)
			}

			if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: planted.structureID}); err != nil {
				t.Fatalf("RemoveStructure: %v", err)
			}
			if standing := len(h.structures()); standing != 0 {
				t.Errorf("%d structures stand in the world, want none", standing)
			}

			dropped := h.sim.sortedDropsLocked()
			if len(dropped) != 1 {
				t.Fatalf("%d drops lie in the world, want the %s that was taken back", len(dropped), tc.name)
			}
			if dropped[0].item != tc.item || dropped[0].count != 1 {
				t.Errorf("the drop is %d of item %d, want 1 %s", dropped[0].count, dropped[0].item, tc.name)
			}
			if want := dropSpawnPos([3]int64{0, tc.spawnVoxelY, 0}); dropped[0].pos != want {
				t.Errorf("the %s dropped at %v, want free space above its support at %v", tc.name, dropped[0].pos, want)
			}
			if overlaps(h.world, dropped[0].box()) {
				t.Errorf("the %s drop begins inside its supporting terrain at %v", tc.name, dropped[0].pos)
			}

			// The existing ten-tick delay is long enough for the drop to settle and be
			// streamed before the nearby owner may collect it.
			h.advance(dropPickupDelayTicks)
			if got := dropped[0].pos[1]; math.Abs(got-tc.restingFloor) > dropTolerance {
				t.Errorf("the %s drop came to rest at y=%v, want the terrain surface at y=%v", tc.name, got, tc.restingFloor)
			}
			seen := out.snapshotDrops(t)
			if len(seen) != 1 || seen[0].ItemID != uint16(tc.item) || seen[0].Pos != dropped[0].wirePos() {
				t.Errorf("the settled %s drop snapshot is %+v, want item %d at %v", tc.name, seen, tc.item, dropped[0].wirePos())
			}
		})
	}
}

// The voxel whose top would cross worldLimit is not free space, even when a terrain
// double reports air there. Manual removal must fail closed instead of minting a drop
// the collision and snapshot arithmetic cannot represent inside the world.
func TestAStructureDropDoesNotSearchPastTheWorldEdge(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	h.sim.mu.Lock()
	_, found := h.sim.firstFreeVoxelAboveLocked([3]int64{0, worldLimit - 1, 0})
	h.sim.mu.Unlock()
	if found {
		t.Error("a structure drop found free space past the upper world edge")
	}
}

// The three refusals removal has, each silent and each leaving the structure standing.
func TestAStructureRemovalTheSimulationRefuses(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		id      func(planted *structure) uint64
		remover func(h *structureHarness, owner *Player) *Player
	}{
		{
			name: "an id that names nothing",
			id:   func(planted *structure) uint64 { return planted.structureID + 1000 },
		},
		{
			name: "the reserved id zero, which is what an absent field decodes as",
			id:   func(*structure) uint64 { return 0 },
		},
		{
			name: "a structure somebody else placed",
			id:   func(planted *structure) uint64 { return planted.structureID },
			remover: func(h *structureHarness, _ *Player) *Player {
				other, _ := h.join(2, [3]float32{0.5, 64, 0.5})
				return other
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			planted := h.plantTent(owner, [3]int32{0, 63, 0})

			remover := owner
			if tc.remover != nil {
				remover = tc.remover(h, owner)
			}
			if err := remover.RemoveStructure(protocol.RemoveStructureRequest{StructureID: tc.id(planted)}); err == nil {
				t.Fatal("the removal was accepted")
			}
			if standing := len(h.structures()); standing != 1 {
				t.Errorf("%d structures stand in the world, want the one that was planted", standing)
			}
			if drops := len(h.sim.sortedDropsLocked()); drops != 0 {
				t.Errorf("%d drops lie in the world, want none", drops)
			}
		})
	}
}

// Removal obeys the reach a block edit does, measured from the same body centre. A player
// who walked away cannot pick their camp up from across the valley.
func TestAStructureOutOfReachIsNotRemovable(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	planted := h.plantTent(player, [3]int32{0, 63, 0})

	h.sim.mu.Lock()
	player.pos = [3]float64{0.5, 64, 40.5}
	h.sim.mu.Unlock()

	if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: planted.structureID}); err == nil {
		t.Fatal("a structure forty blocks away was removed")
	}
	if standing := len(h.structures()); standing != 1 {
		t.Errorf("%d structures stand in the world, want the one that was planted", standing)
	}
}

// ---------------------------------------------------------------------------
// Collapse
// ---------------------------------------------------------------------------

// Breaking any one of the cells a structure rests on brings it down and leaves its item
// where it stood. Every supporting cell of both kinds, because "any" is the claim.
func TestBreakingAnySupportingBlockCollapsesTheStructure(t *testing.T) {
	t.Parallel()

	for _, kind := range []struct {
		name   string
		item   ItemID
		wanted vnet.StructureKind
	}{
		{"tent", ItemTent, vnet.StructureKindTent},
		{"forge", ItemForge, vnet.StructureKindForge},
		{"campfire", ItemCampfire, vnet.StructureKindCampfire},
		{"runestone", ItemRunestone, vnet.StructureKindRunestone},
	} {
		anchor := [3]int64{0, 63, 0}
		cells, _, _ := footprintOf(kind.wanted, vnet.FacingNorth, anchor)

		for _, cell := range cells {
			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(player, 0, kind.item, 1)
			if _, _, err := player.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth)); err != nil {
				t.Fatalf("%s: %v", kind.name, err)
			}

			broken := [3]int32{int32(cell[0]), int32(cell[1]), int32(cell[2])}
			if _, err := player.breakMined(context.Background(), broken, world.Stone); err != nil {
				t.Fatalf("%s: breaking %v: %v", kind.name, cell, err)
			}

			if standing := len(h.structures()); standing != 0 {
				t.Errorf("%s: breaking %v left %d structures standing", kind.name, cell, standing)
			}
			dropped := h.sim.sortedDropsLocked()
			// Two: the stone that was broken, and the structure that was resting on it.
			if len(dropped) != 2 {
				t.Fatalf("%s: breaking %v left %d drops, want the stone and the structure", kind.name, cell, len(dropped))
			}
			if !holdsDropOf(dropped, kind.item) {
				t.Errorf("%s: breaking %v dropped no %d", kind.name, cell, kind.item)
			}
		}
	}
}

// A block beside the footprint is not a support, and breaking it changes nothing. The
// negative half of the rule above, and the one that says "any occupied cell" is not "any
// block nearby".
func TestBreakingABlockBesideTheFootprintLeavesTheStructureStanding(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantTent(player, [3]int32{0, 63, 0})

	for _, beside := range [][3]int32{
		{2, 63, 0},  // one cell past the tent's edge
		{0, 62, 0},  // directly under the anchor, one layer down
		{1, 63, -2}, // past the corner
	} {
		if _, err := player.breakMined(context.Background(), beside, world.Stone); err != nil {
			t.Fatalf("breaking %v: %v", beside, err)
		}
		if standing := len(h.structures()); standing != 1 {
			t.Fatalf("breaking %v brought the tent down", beside)
		}
	}
}

func holdsDropOf(drops []*itemDrop, item ItemID) bool {
	for _, d := range drops {
		if d.item == item {
			return true
		}
	}
	return false
}

// ---------------------------------------------------------------------------
// Respawn
// ---------------------------------------------------------------------------

// The policy respawnLocked's comment used to promise: a player with a tent comes back
// standing on its anchor, and one without comes back where they joined.
func TestADeadPlayerComesBackAtTheirTentAndOtherwiseAtTheJoinSpawn(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// No tent yet: the join spawn, exactly as before this issue.
	//
	// The column rather than the exact position, here and below. The respawn puts the
	// player above the ground with onGround false and the tick settles them onto it, so
	// the height that survives is the collision skin's answer rather than the one the
	// policy chose — and the policy is what these tests are about.
	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()
	h.advance(int(h.sim.deathTicks) + 1)
	if got := playerPosition(h.sim, player); got[0] != 0.5 || got[2] != 0.5 {
		t.Errorf("with no tent the player came back at %v, want the join spawn column at x 0.5, z 0.5", got)
	}

	// Walk away, plant a tent, die: back at the tent.
	h.sim.mu.Lock()
	player.pos = [3]float64{20.5, 64, 20.5}
	h.sim.mu.Unlock()
	h.plantTent(player, [3]int32{20, 63, 20})

	h.sim.mu.Lock()
	player.pos = [3]float64{0.5, 64, 0.5}
	player.protectionTicks = 0
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()
	h.advance(int(h.sim.deathTicks) + 1)

	// One block above the anchor, centred in the cell: standing on the ground the
	// footprint guaranteed was solid, inside the headroom it guaranteed was clear.
	// Read after the settle, so this is where the tick left them rather than only where
	// the respawn put them.
	if got := playerPosition(h.sim, player); got[0] != 20.5 || got[2] != 20.5 {
		t.Errorf("the player came back at %v, want the tent's column at x 20.5, z 20.5", got)
	}
}

// Once the tent has gone the fallback comes back — and it is read from the live registry,
// so neither road to "no tent" can leave a position dangling.
func TestWithoutATentTheJoinSpawnIsTheFallbackAgain(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name   string
		unmake func(h *structureHarness, p *Player, planted *structure)
	}{
		{
			name: "the owner took it back",
			unmake: func(_ *structureHarness, p *Player, planted *structure) {
				if err := p.RemoveStructure(protocol.RemoveStructureRequest{StructureID: planted.structureID}); err != nil {
					panic(err)
				}
			},
		},
		{
			name: "somebody dug the ground out from under it",
			unmake: func(_ *structureHarness, p *Player, _ *structure) {
				if _, err := p.breakMined(context.Background(), [3]int32{20, 63, 20}, world.Stone); err != nil {
					panic(err)
				}
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

			h.sim.mu.Lock()
			player.pos = [3]float64{20.5, 64, 20.5}
			h.sim.mu.Unlock()
			planted := h.plantTent(player, [3]int32{20, 63, 20})
			tc.unmake(h, player, planted)

			h.sim.mu.Lock()
			player.damageLocked(PlayerMaxHealth)
			h.sim.mu.Unlock()
			h.advance(int(h.sim.deathTicks) + 1)

			if got := playerPosition(h.sim, player); got[0] != 0.5 || got[2] != 0.5 {
				t.Errorf("the player came back at %v, want the join spawn column at x 0.5, z 0.5", got)
			}
		})
	}
}

func playerPosition(sim *Sim, p *Player) [3]float64 {
	sim.mu.Lock()
	defer sim.mu.Unlock()
	return p.pos
}

// ---------------------------------------------------------------------------
// The snapshot
// ---------------------------------------------------------------------------

// A structure inside the cube a session is streaming is in its snapshot; one outside it is
// not, and one that stops standing simply stops appearing. The complete-existence-set
// rule, executed rather than documented.
func TestASnapshotCarriesTheStructuresASessionCanSeeAndNoOthers(t *testing.T) {
	t.Parallel()

	// A view distance of one, not zero. A structure is placed in the chunk of the ground
	// it rests on, and a player standing on that ground is in the chunk *above* whenever
	// the anchor is the last block of one — which is exactly the case here. One chunk of
	// radius is the smallest cube that holds both, and it is still small enough to walk
	// out of.
	h := newStructureHarnessAt(t, 1)
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	planted := h.plantTent(player, [3]int32{0, 63, 0})

	h.step()
	if got := snapshotStructures(t, out); len(got) != 1 || got[0].StructureId() != planted.structureID {
		t.Fatalf("the session was sent %d structures, want the one standing beside it", len(got))
	}

	// Far enough that the tent's chunk is outside the cube.
	h.sim.mu.Lock()
	player.pos = [3]float64{0.5, 64, 100.5}
	player.chunk = chunkAt(player.pos)
	h.sim.mu.Unlock()
	h.step()
	if got := snapshotStructures(t, out); len(got) != 0 {
		t.Errorf("the session was sent %d structures from a chunk it does not hold, want none", len(got))
	}

	// Back in view, then taken back: the vector carries it and then stops, and nothing on
	// the wire says which of the two reasons applied.
	h.sim.mu.Lock()
	player.pos = [3]float64{0.5, 64, 0.5}
	player.chunk = chunkAt(player.pos)
	h.sim.mu.Unlock()
	h.step()
	if got := snapshotStructures(t, out); len(got) != 1 {
		t.Fatalf("the session was sent %d structures after walking back, want 1", len(got))
	}

	if err := player.RemoveStructure(protocol.RemoveStructureRequest{StructureID: planted.structureID}); err != nil {
		t.Fatalf("RemoveStructure: %v", err)
	}
	h.step()
	if got := snapshotStructures(t, out); len(got) != 0 {
		t.Errorf("the session was sent %d structures after the tent came down, want none", len(got))
	}
}

// snapshotStructures is the structure vector of the newest snapshot this session was sent.
func snapshotStructures(t *testing.T, out *dropSink) []*vnet.StructureState {
	t.Helper()

	snapshot := newestSnapshot(t, out)
	states := make([]*vnet.StructureState, 0, snapshot.StructuresLength())
	for i := range snapshot.StructuresLength() {
		state := new(vnet.StructureState)
		if !snapshot.Structures(state, i) {
			t.Fatalf("structure %d is missing from a snapshot that claims to hold it", i)
		}
		states = append(states, state)
	}
	return states
}

// Every refusal a placement can produce names its own reason, and the two groups the
// contract splits them into stay on their own sides of the line.
//
// **The reason is what the player is told, so it is the half that has to be right.** The
// error beside it is prose for a server log and names the exact cell; nothing on the wire
// reads it, and nothing here checks its wording. What this pins is the code: which of the
// fifteen members of RefusalReason each path answers with, that no two paths collapse onto
// one, and that a refusal about the *request* never reads as a refusal by the *world* —
// which is the whole reason schemas/player.fbs splits the enum by value.
//
// Three of the fifteen are deliberately absent and stay absent honestly rather than being
// faked with a seam. MalformedKind is unreachable by construction (knownStructureKind and
// footprintOf switch over the same kinds), and SlotChanged and InventoryBusy both require
// the pack to move between the snapshot this function takes and the spend it makes — a
// race, not a state a single goroutine can arrange. They are covered by the grouping sweep
// below and by nothing stronger, which is the truth about them.
func TestEveryPlacementRefusalNamesItsOwnReason(t *testing.T) {
	t.Parallel()

	// The value at which schemas/player.fbs stops describing a world that said no and
	// starts describing a request no correct client sends. Written down here rather than
	// derived, so that a reason appended to the wrong group fails this test instead of
	// reaching a client that would explain a build's defect to a player.
	const firstMalformed = vnet.RefusalReasonMalformedNoAnchor

	for _, tc := range []struct {
		name    string
		item    ItemID
		request protocol.PlaceStructureRequest
		prepare func(h *structureHarness, p *Player)
		want    vnet.RefusalReason
	}{
		{
			name:    "no anchor at all",
			item:    ItemTent,
			request: protocol.PlaceStructureRequest{Slot: 0, Facing: vnet.FacingNorth},
			want:    vnet.RefusalReasonMalformedNoAnchor,
		},
		{
			name:    "the absent-field facing",
			item:    ItemTent,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingUnknown),
			want:    vnet.RefusalReasonMalformedFacing,
		},
		{
			name:    "a facing no member of the enum has",
			item:    ItemTent,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.Facing(99)),
			want:    vnet.RefusalReasonMalformedFacing,
		},
		{
			name:    "a slot outside the announced inventory",
			item:    ItemTent,
			request: placeRequest(protocol.InventorySlots, [3]int32{0, 63, 0}, vnet.FacingNorth),
			want:    vnet.RefusalReasonMalformedSlot,
		},
		{
			name:    "a slot with nothing in it",
			item:    ItemTent,
			request: placeRequest(7, [3]int32{0, 63, 0}, vnet.FacingNorth),
			want:    vnet.RefusalReasonSlotEmpty,
		},
		{
			name:    "a slot holding something that plants no structure",
			item:    ItemTent,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) { h.give(p, 0, ItemStone, 8) },
			want:    vnet.RefusalReasonSlotUnusable,
		},
		{
			name:    "a dead player",
			item:    ItemTent,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) {
				h.sim.mu.Lock()
				defer h.sim.mu.Unlock()
				p.damageLocked(PlayerMaxHealth)
			},
			want: vnet.RefusalReasonPlayerIsDead,
		},
		{
			name:    "an anchor past the reach a block edit obeys",
			item:    ItemTent,
			request: placeRequest(0, [3]int32{0, 63, 40}, vnet.FacingNorth),
			want:    vnet.RefusalReasonOutOfReach,
		},
		{
			name:    "ground the server has not composed yet",
			item:    ItemCampfire,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) { h.world.hide([3]int64{0, 63, 0}) },
			want:    vnet.RefusalReasonGroundNotGenerated,
		},
		{
			name:    "ground that is air",
			item:    ItemCampfire,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) { h.world.set([3]int64{0, 63, 0}, world.Air) },
			want:    vnet.RefusalReasonGroundIsAir,
		},
		{
			name:    "headroom the server has not composed yet",
			item:    ItemCampfire,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) { h.world.hide([3]int64{0, 64, 0}) },
			want:    vnet.RefusalReasonSpaceNotGenerated,
		},
		{
			name:    "a block standing in the headroom",
			item:    ItemCampfire,
			request: placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) { h.world.set([3]int64{0, 64, 0}, world.Stone) },
			want:    vnet.RefusalReasonSpaceBlocked,
		},
		{
			name:    "a second tent while the first still stands",
			item:    ItemTent,
			request: placeRequest(1, [3]int32{3, 63, 0}, vnet.FacingNorth),
			prepare: func(h *structureHarness, p *Player) {
				h.plantTent(p, [3]int32{0, 63, 0})
				h.give(p, 1, ItemTent, 1)
			},
			want: vnet.RefusalReasonTentAlreadyPlaced,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(player, 0, tc.item, 1)
			if tc.prepare != nil {
				tc.prepare(h, player)
			}

			_, reason, err := player.PlaceStructure(tc.request)
			if err == nil {
				t.Fatal("the placement was accepted")
			}
			if reason != tc.want {
				t.Errorf("reason = %s, want %s (error: %v)", reason, tc.want, err)
			}
			if reason == vnet.RefusalReasonUnknown {
				t.Error("a refusal answered Unknown, which is the absent-field value and says nothing")
			}
		})
	}

	// The grouping, over every reason a placement can name — the three unreachable ones
	// included, which is the only place they are checked at all. A member appended to the
	// wrong side of the line fails here, and that failure matters: a client shows the
	// world's answers to the player and its own defects to a log, and it tells the two
	// apart by this split alone.
	for _, reason := range []vnet.RefusalReason{
		vnet.RefusalReasonGroundNotGenerated,
		vnet.RefusalReasonGroundIsAir,
		vnet.RefusalReasonSpaceNotGenerated,
		vnet.RefusalReasonSpaceBlocked,
		vnet.RefusalReasonOutOfReach,
		vnet.RefusalReasonPlayerIsDead,
		vnet.RefusalReasonSlotEmpty,
		vnet.RefusalReasonSlotUnusable,
		vnet.RefusalReasonSlotChanged,
		vnet.RefusalReasonInventoryBusy,
		vnet.RefusalReasonTentAlreadyPlaced,
		// V20's chat and party refusals share the same world/state group even though
		// PlaceStructure never produces them.
		vnet.RefusalReasonTooFast,
		vnet.RefusalReasonPartyFull,
		vnet.RefusalReasonNoSuchPlayer,
		vnet.RefusalReasonAlreadyInParty,
		vnet.RefusalReasonNoInvite,
		vnet.RefusalReasonNotLeader,
		// V21 reserves the authoritative corpse-loot answers for the dependent loot
		// simulation issue. They are still world/state refusals, never client defects.
		vnet.RefusalReasonCorpseUnavailable,
		vnet.RefusalReasonLootNotOwned,
		vnet.RefusalReasonStaleRevision,
		vnet.RefusalReasonInventoryFull,
		// V22 reserves this authoritative launcher refusal for the dependent bow
		// issue. It is still the player's own inventory state, never a client defect.
		vnet.RefusalReasonNoAmmunition,
		// V24 reserves the map's four refusals for the dependent server issues. Every one
		// of them is a legal question the world answered no to — a grid the client should
		// ask again on, a map that is full, a note the player should shorten, a mark that
		// is not theirs — so all four belong in the low group, and schemas/player.fbs
		// carries the argument for the two that read like peer defects at first glance.
		vnet.RefusalReasonTileMisaligned,
		vnet.RefusalReasonTooManyMarkers,
		vnet.RefusalReasonNoteTooLong,
		vnet.RefusalReasonMarkerUnknown,
		// V25 reserves the settlement's three for the dependent server issues, and all
		// three are the world answering a legal question no: a resident who keeps no
		// stall, a purse that is short, an item this vendor does not deal in. The first
		// is the one worth naming — nothing on the wire says which residents trade until
		// one is addressed, so a client cannot compute that answer for itself and it is
		// not a defect that it asked.
		vnet.RefusalReasonNotAVendor,
		vnet.RefusalReasonNotEnoughSilver,
		vnet.RefusalReasonVendorDoesNotWant,
		// V26 reserves one for the runestone's ward, and it is the same shape: the ground
		// belongs to somebody else, the request was well formed, and the player can walk
		// somewhere else. It names no owner, deliberately — an answer that did would let
		// a client learn who has claimed ground by poking at it.
		vnet.RefusalReasonWarded,
		// V27 reserves the stable's authoritative mount and cast answers. Producers and
		// presentation are intentionally split into the dependent feature issues.
		vnet.RefusalReasonMountNotLearned,
		vnet.RefusalReasonAlreadyMounted,
		vnet.RefusalReasonMountNotGrounded,
		vnet.RefusalReasonMountIndoors,
		vnet.RefusalReasonMountLowCeiling,
		vnet.RefusalReasonCastAlreadyInProgress,
		vnet.RefusalReasonCastInterruptedByDamage,
		vnet.RefusalReasonCastInterruptedByMovement,
		vnet.RefusalReasonCastInterruptedByJump,
		vnet.RefusalReasonCastInterruptedByDeath,
		vnet.RefusalReasonActionForbiddenWhileMounted,
		vnet.RefusalReasonMountAlreadyLearned,
	} {
		if reason == vnet.RefusalReasonUnknown || reason >= firstMalformed {
			t.Errorf("%s is answered by a world that said no, so it belongs in 1..%d", reason, firstMalformed-1)
		}
	}
	for _, reason := range []vnet.RefusalReason{
		vnet.RefusalReasonMalformedNoAnchor,
		vnet.RefusalReasonMalformedFacing,
		vnet.RefusalReasonMalformedSlot,
		vnet.RefusalReasonMalformedKind,
	} {
		if reason < firstMalformed {
			t.Errorf("%s names a request no correct client sends, so it belongs at %d or above", reason, firstMalformed)
		}
	}

	// Membership, in the shape the Payload union is checked in over in protocol: a reason
	// added without a decision fails here. V21's four loot members have their client
	// vocabulary now and receive producers in the dependent authoritative loot issue;
	// V22's ammunition refusal follows the same staged-contract pattern for the bow.
	// The count includes Unknown.
	if got := len(vnet.EnumNamesRefusalReason); got != 47 {
		t.Errorf("RefusalReason has %d members, want 47 — a new one needs a producer and client handling, not a test edit", got)
	}
}

// A placement that succeeds names no reason at all.
//
// Unknown is the zero value and means "nothing was refused" here, which is why nothing is
// ever sent on this path: the session only encodes an ActionRefused when the error is
// non-nil, and a reason that arrived beside a success would be a refusal the player was
// told about for a structure that is standing in front of them.
func TestAnAcceptedPlacementNamesNoReason(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.give(player, 0, ItemCampfire, 1)

	_, reason, err := player.PlaceStructure(placeRequest(0, [3]int32{0, 63, 0}, vnet.FacingNorth))
	if err != nil {
		t.Fatalf("PlaceStructure: %v", err)
	}
	if reason != vnet.RefusalReasonUnknown {
		t.Errorf("reason = %s, want Unknown — nothing was refused", reason)
	}
}
