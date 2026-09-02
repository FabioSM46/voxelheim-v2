package game_test

import (
	"bytes"
	"context"
	"errors"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The seed the edit tests build their world from. Any seed works — nothing here asserts a
// height — but a fixed one keeps a failure reproducible.
const editSeed = 4242

// refusingEditor is an Editor that fails every write.
//
// The movement tests never edit, and this is how they say so: NewSim refuses a nil editor,
// and handing a file about walking a working one would let a broken edit path go unnoticed
// there. It also covers the one refusal Player.Edit cannot manufacture on its own — a world
// that will not accept the write.
type refusingEditor struct{}

var errEditorRefused = errors.New("this editor refuses every edit")

func (refusingEditor) ApplyGuarded(context.Context, int64, int64, int64, world.Block, func() error, func(world.Block) error) error {
	return errEditorRefused
}

// editWorld builds a simulation over a real chunk cache, with both seams pointing at it:
// collision reads it through Peek and edits are applied to it directly. That is the wiring
// main.go produces, and it is the only wiring in which "collision sees edits" means
// anything.
// openCountrySpawn is where a test that edits the ground stands its session, and it is
// deliberately not [world.SpawnAt].
//
// **Since #519 the join spawn is the capital's gate square, and a settlement wards every
// column of its plateau against every player**, so a mining test standing there would be
// measuring [game.ErrWarded] rather than the path it is about. The origin column is open
// country no settlement reaches, the capital standing at least 120 blocks away.
func openCountrySpawn(seed int64) [3]float32 {
	return [3]float32{0.5, float32(world.GeneratedColumnTop(seed, 0, 0) + world.SpawnClearance), 0.5}
}

func editWorld(t *testing.T) (*harness, *world.Cache) {
	t.Helper()

	chunks := world.NewCache(editSeed, 2, 256)
	sim, err := game.NewSim(game.DefaultTickRate, 2, testWorldSeed, game.NewCacheTerrain(chunks), chunks, testEntityIDs(), discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureWater(chunks); err != nil {
		t.Fatalf("ConfigureWater: %v", err)
	}
	return &harness{t: t, sim: sim}, chunks
}

// generateAround makes the chunks around a position resident, because the tick loop reads
// terrain with Peek and never generates any of its own.
func generateAround(t *testing.T, chunks *world.Cache, pos [3]float32, radius int32) {
	t.Helper()

	center := world.ContainingChunk(pos[0], pos[1], pos[2])
	for y := center.Y - radius; y <= center.Y+radius; y++ {
		for z := center.Z - radius; z <= center.Z+radius; z++ {
			for x := center.X - radius; x <= center.X+radius; x++ {
				if _, _, err := chunks.Get(context.Background(), world.Coord{X: x, Y: y, Z: z}); err != nil {
					t.Fatalf("Get(%d,%d,%d): %v", x, y, z, err)
				}
			}
		}
	}
}

// blockAt reads one voxel through the cache, generating its chunk if it has to.
func blockAt(t *testing.T, chunks *world.Cache, x, y, z int64) world.Block {
	t.Helper()

	block, err := chunks.BlockAt(context.Background(), x, y, z)
	if err != nil {
		t.Fatalf("BlockAt(%d,%d,%d): %v", x, y, z, err)
	}
	return block
}

func placeFromSlot(pos [3]int32, slot uint8) protocol.BlockEditRequest {
	return protocol.BlockEditRequest{Pos: pos, HasPos: true, Action: vnet.EditActionPlace, Slot: slot}
}

// mineAt drives the public mining seam exactly as a session does: refresh intent,
// let Step pay one tick, then perform the opaque completion off-tick. It is shared by
// edit/inventory tests whose setup needs a real authoritative drop.
func mineAt(t *testing.T, h *harness, player *game.Player, pos [3]int32) game.EditResult {
	t.Helper()

	type nextResult struct {
		completion game.MiningCompletion
		err        error
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	ready := make(chan nextResult, 1)
	go func() {
		completion, err := player.NextMining(ctx)
		ready <- nextResult{completion: completion, err: err}
	}()
	finish := func(next nextResult) game.EditResult {
		if next.err != nil {
			t.Fatalf("wait for mining at %v: %v", pos, next.err)
		}
		result, err := player.CompleteMining(context.Background(), next.completion)
		if err != nil {
			t.Fatalf("complete mining at %v: %v", pos, err)
		}
		return result
	}

	// Generous against the slowest block in the table rather than a round number: iron ore
	// costs eight seconds by hand, and a bound that happened to sit above it before #178
	// raised the table is a bound that fails the next time somebody retunes one.
	const budget = 400
	for range budget {
		h.clientTick++
		request := protocol.MineRequest{Pos: pos, HasPos: true, Active: true, ClientTick: h.clientTick}
		if err := player.Mine(request, true); err != nil {
			// Step may have put the opaque completion in its bounded handoff before
			// the goroutine above gets scheduled to receive it. In that case Mine
			// correctly refuses a new refresh; wait for the already-paid result instead
			// of making goroutine scheduling part of the assertion.
			select {
			case next := <-ready:
				return finish(next)
			case <-time.After(time.Second):
				t.Fatalf("refresh mining at %v: %v", pos, err)
			}
		}
		h.step()

		select {
		case next := <-ready:
			return finish(next)
		default:
		}
	}

	t.Fatalf("mining at %v did not complete in %d ticks", pos, budget)
	return game.EditResult{}
}

func placeAt(t *testing.T, player *game.Player, pos [3]int32, block world.Block) protocol.BlockEditRequest {
	t.Helper()
	itemID := itemThatPlaces(t, block)
	for slot, stack := range player.InventoryState().Stacks {
		if stack.ItemID == uint16(itemID) && stack.Count > 0 {
			return placeFromSlot(pos, uint8(slot))
		}
	}
	t.Fatalf("the inventory has no block %d to select", block)
	return protocol.BlockEditRequest{}
}

func itemThatPlaces(t *testing.T, block world.Block) game.ItemID {
	t.Helper()
	switch block {
	case world.Stone:
		return game.ItemStone
	case world.Dirt:
		return game.ItemDirt
	case world.Snow:
		return game.ItemSnow
	case world.Log:
		return game.ItemLog
	default:
		t.Fatalf("test helper has no placeable item for block %d", block)
		return game.ItemNone
	}
}

// dropShelf is the voxel beside the player's feet, and the one under it.
//
// Breaking the first is legal — it is inside EditReach and outside the body — and the
// second is what the yield comes to rest on, one player-width away, which is inside
// game.DropPickupRadius. Derived from the player's own position rather than written
// out, because these tests stand at more than one height.
func dropShelf(t *testing.T, player *game.Player) (target, floor [3]int32) {
	t.Helper()

	feet := player.State().Pos
	target = [3]int32{
		int32(math.Floor(float64(feet[0]))) - 1,
		int32(math.Floor(float64(feet[1]))),
		int32(math.Floor(float64(feet[2]))),
	}
	return target, [3]int32{target[0], target[1] - 1, target[2]}
}

// giveBlock prepares one item through the operations a player performs in the game:
// break the block, then stand next to what it left on the ground until it is collected.
//
// The direct cache writes are fixture setup only. The inventory still has no test back
// door — and after this issue a break is not one either, so every count in every test
// below entered through an authoritative pickup on the tick.
func giveBlock(t *testing.T, h *harness, player *game.Player, chunks *world.Cache, block world.Block) {
	t.Helper()

	item := itemYieldedBy(t, block)
	before := countOf(player.InventoryState(), item)

	target, floor := dropShelf(t, player)
	if err := chunks.Apply(context.Background(), int64(floor[0]), int64(floor[1]), int64(floor[2]), world.Stone, nil); err != nil {
		t.Fatalf("prepare the shelf under the drop: %v", err)
	}
	if err := chunks.Apply(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]), block, nil); err != nil {
		t.Fatalf("prepare block %d to carry: %v", block, err)
	}

	mineAt(t, h, player, target)

	// A wait rather than an assertion, because a drop is deliberately not collectable
	// on the tick it appears: the pickup delay is the whole difference between an item
	// you see fall and a number that changes.
	for range 200 {
		h.step()
		if countOf(player.InventoryState(), item) > before {
			return
		}
	}
	t.Fatalf("breaking block %d left a drop that never reached the inventory", block)
}

// itemYieldedBy mirrors the server's drop table for the fixtures above.
func itemYieldedBy(t *testing.T, block world.Block) game.ItemID {
	t.Helper()
	switch block {
	case world.Stone:
		return game.ItemStone
	case world.Dirt, world.Grass:
		return game.ItemDirt
	case world.Snow:
		return game.ItemSnow
	case world.Log:
		return game.ItemLog
	case world.CoalOre:
		return game.ItemRawCoal
	case world.IronOre:
		return game.ItemRawIron
	default:
		t.Fatalf("test helper has no drop for block %d", block)
		return game.ItemNone
	}
}

func countOf(state protocol.InventoryState, itemID game.ItemID) uint16 {
	for _, stack := range state.Stacks {
		if stack.ItemID == uint16(itemID) {
			return stack.Count
		}
	}
	return 0
}

// ---------------------------------------------------------------------------
// Reach
// ---------------------------------------------------------------------------

// The reach limit, at the granularity a player experiences it: the last voxel inside it
// and the first one outside. Both are placements into open air well away from the body, so
// the only thing separating them is the distance.
func TestEditReachAcceptsTheLastVoxelInsideItAndRefusesTheFirstOutside(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	// High above any terrain this seed can produce, so every target is air and nothing
	// here depends on the shape of the ground.
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	giveBlock(t, h, player, chunks, world.Stone)
	giveBlock(t, h, player, chunks, world.Stone)

	// From the centre of the body at (0.5, 200.9, 0.5): 4.02 blocks to the first target,
	// 5.02 to the second, with game.EditReach between them.
	inside := [3]int32{4, 200, 0}
	outside := [3]int32{5, 200, 0}

	if _, err := player.Edit(context.Background(), placeAt(t, player, inside, world.Stone)); err != nil {
		t.Fatalf("a voxel 4.02 blocks away was refused, and the reach is %.1f: %v", game.EditReach, err)
	}
	if got := blockAt(t, chunks, 4, 200, 0); got != world.Stone {
		t.Errorf("the accepted edit left block %d at the target, want Stone", got)
	}

	if _, err := player.Edit(context.Background(), placeAt(t, player, outside, world.Stone)); err == nil {
		t.Fatalf("a voxel 5.02 blocks away was accepted, and the reach is %.1f", game.EditReach)
	}
	if got := blockAt(t, chunks, 5, 200, 0); got != world.Air {
		t.Errorf("a refused edit changed the world: block %d stands at the target", got)
	}
}

// pos is unbounded on the wire, so the reach check has to run *before* the world is
// touched. If it did not, naming a voxel on the far side of the world would be enough to
// make the server generate the chunk around it — for free, from an unadmitted request's
// worth of bytes.
func TestAnUnreachableEditGeneratesNothing(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})

	before := chunks.Len()
	h.clientTick++
	if err := player.Mine(protocol.MineRequest{
		Pos: [3]int32{1 << 20, 1 << 20, 1 << 20}, HasPos: true, Active: true, ClientTick: h.clientTick,
	}, true); err == nil {
		t.Fatal("an edit a million blocks away was accepted")
	}
	if after := chunks.Len(); after != before {
		t.Errorf("the cache grew from %d to %d chunks resolving an edit that was never in reach", before, after)
	}
}

// ---------------------------------------------------------------------------
// Legality
// ---------------------------------------------------------------------------

func TestBreakingAirIsRefused(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})

	// Two blocks above the player's head, and nothing up here is solid. Read it once
	// so the non-blocking mining check can distinguish resident Air from a cache miss.
	if got := blockAt(t, chunks, 0, 203, 0); got != world.Air {
		t.Fatalf("fixture target holds block %d, want Air", got)
	}
	h.clientTick++
	if err := player.Mine(protocol.MineRequest{
		Pos: [3]int32{0, 203, 0}, HasPos: true, Active: true, ClientTick: h.clientTick,
	}, true); err == nil {
		t.Fatal("breaking a voxel that holds air was accepted; there was nothing there to break")
	}
}

func TestPlacingIntoASolidVoxelIsRefused(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	ctx := context.Background()
	giveBlock(t, h, player, chunks, world.Stone)
	giveBlock(t, h, player, chunks, world.Dirt)

	target := [3]int32{3, 200, 0}
	if _, err := player.Edit(ctx, placeAt(t, player, target, world.Stone)); err != nil {
		t.Fatalf("the first placement was refused: %v", err)
	}

	// The same voxel again, with a different block. A placement is not a replacement.
	if _, err := player.Edit(ctx, placeAt(t, player, target, world.Dirt)); err == nil {
		t.Fatal("a placement into an occupied voxel was accepted")
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Stone {
		t.Errorf("the refused placement overwrote the voxel: block %d stands there, want Stone", got)
	}
}

// Water is displaced by a placement rather than obstructing it, and the placement is
// one ordinary edit — one item spent, one voxel changed.
//
// **The water is scripted here rather than generated, and that is the point of the
// split this pull request is half of.** Nothing in this build puts water in the
// ground yet; what exists is the id, the passability rule and the legality test, and
// those are exactly what this exercises. The generator's own water arrives with the
// worldgen half, and the fixture writes below are the same direct cache writes
// `giveBlock` already uses to stage a block to carry.
func TestPlacingIntoFlowingWaterReplacesIt(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	// The fixture sits on a chunk border, and since #717 a water voxel with an unread
	// neighbour is deferred rather than decided from fabricated blocks. This test is
	// about the edit, not the residency frontier, so the one chunk the neighbourhood
	// crosses into is made resident — and only that one: the chunks under the player
	// stay unread, because unread is what stands the fixture's floor up.
	if _, _, err := chunks.Get(context.Background(), world.Coord{Y: 6, Z: -1}); err != nil {
		t.Fatalf("compose the bordering chunk: %v", err)
	}
	giveBlock(t, h, player, chunks, world.Stone)

	target := [3]int32{3, 200, 0}
	if err := chunks.Apply(context.Background(), 4, 199, 0, world.Stone, nil); err != nil {
		t.Fatalf("support neighbouring flow: %v", err)
	}
	if err := chunks.Apply(context.Background(), 4, 200, 0, world.WaterFlow3, nil); err != nil {
		t.Fatalf("prepare neighbouring flow: %v", err)
	}
	if err := chunks.Apply(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]), world.WaterFlow3, nil); err != nil {
		t.Fatalf("flood the target voxel: %v", err)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.WaterFlow3 {
		t.Fatalf("the fixture target holds block %d, want WaterFlow3", got)
	}

	before := countOf(player.InventoryState(), game.ItemStone)
	result, err := player.Edit(context.Background(), placeAt(t, player, target, world.Stone))
	if err != nil {
		t.Fatalf("placing Stone into water was refused: %v", err)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Stone {
		t.Errorf("the target voxel holds block %d after the placement, want Stone", got)
	}
	if result.Inventory == nil {
		t.Fatal("the accepted placement reported no inventory change")
	}
	if got := countOf(*result.Inventory, game.ItemStone); got != before-1 {
		t.Errorf("Stone count after placing = %d, want %d: a placement into water spends one block like any other", got, before-1)
	}
	h.step()
	if got := blockAt(t, chunks, 4, 200, 0); got != world.Air {
		t.Errorf("neighbour after placement = %d, want scheduled flow to drain", got)
	}
}

// A flower is displaced by a placement on exactly the terms water is, and sits beside
// the water case for that reason: one rule with two id classes in it. Water schedules
// flow after a placement; ground cover schedules nothing.
func TestPlacingIntoAFlowerReplacesIt(t *testing.T) {
	t.Parallel()

	// One flower: allowPlacement reads world.Cover rather than an id, and world's own
	// palette test pins the class to exactly these three.
	const flower = world.FlowerRed

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	giveBlock(t, h, player, chunks, world.Stone)

	target := [3]int32{3, 200, 0}
	if err := chunks.Apply(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]), flower, nil); err != nil {
		t.Fatalf("grow the target flower: %v", err)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != flower {
		t.Fatalf("the fixture target holds block %d, want block %d", got, flower)
	}

	before := countOf(player.InventoryState(), game.ItemStone)
	result, err := player.Edit(context.Background(), placeAt(t, player, target, world.Stone))
	if err != nil {
		t.Fatalf("placing Stone into a flower was refused: %v", err)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Stone {
		t.Errorf("the target voxel holds block %d after the placement, want Stone", got)
	}
	if result.Inventory == nil {
		t.Fatal("the accepted placement reported no inventory change")
	}
	if got := countOf(*result.Inventory, game.ItemStone); got != before-1 {
		t.Errorf("Stone count after placing = %d, want %d: a placement into a flower spends one block like any other", got, before-1)
	}

	// A new cover id follows the same class rule, not a second id-specific rule.
	h, chunks = editWorld(t)
	player, _ = h.join(2, [3]float32{0.5, 200, 0.5})
	giveBlock(t, h, player, chunks, world.Stone)
	if err := chunks.Apply(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]), world.WinterBramble, nil); err != nil {
		t.Fatalf("grow the target winter bramble: %v", err)
	}
	before = countOf(player.InventoryState(), game.ItemStone)
	result, err = player.Edit(context.Background(), placeAt(t, player, target, world.Stone))
	if err != nil || result.Inventory == nil || countOf(*result.Inventory, game.ItemStone) != before-1 {
		t.Fatalf("replace the winter bramble: result=%+v err=%v", result, err)
	}
}

// A block placed inside a player would leave them stuck: moveAndCollide refuses to move a
// player who is already inside a solid rather than teleporting them out of it.
func TestPlacingInsideAPlayersBoxIsRefused(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	ctx := context.Background()

	// Feet at y=200, so the body occupies y in [200, 201.8) and the voxels at 200 and 201
	// are inside it.
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	giveBlock(t, h, player, chunks, world.Stone)

	for _, y := range []int32{200, 201} {
		if _, err := player.Edit(ctx, placeAt(t, player, [3]int32{0, y, 0}, world.Stone)); err == nil {
			t.Errorf("a block was placed at y=%d, inside the body of the player asking for it", y)
		}
		if got := blockAt(t, chunks, 0, int64(y), 0); got != world.Air {
			t.Errorf("the refused placement at y=%d left block %d behind", y, got)
		}
	}

	// The voxel the player is standing on is *not* inside them: the box is half-open, so a
	// surface a player rests on belongs to the world rather than to the body.
	if _, err := player.Edit(ctx, placeAt(t, player, [3]int32{0, 199, 0}, world.Stone)); err != nil {
		t.Errorf("the voxel under the player's feet was treated as part of the player: %v", err)
	}
}

// The check covers every player, not just the one asking — placing a block inside somebody
// else is the version of this that is worth doing on purpose.
func TestPlacingInsideAnotherPlayersBoxIsRefused(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	editor, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	h.join(2, [3]float32{2.5, 200, 0.5})
	// The occupancy check deliberately precedes the stack check, but give the player
	// the block anyway so this test keeps proving the intended refusal if that ordering
	// ever changes.
	giveBlock(t, h, editor, chunks, world.Stone)

	if _, err := editor.Edit(context.Background(), placeAt(t, editor, [3]int32{2, 200, 0}, world.Stone)); err == nil {
		t.Fatal("a block was placed inside another player")
	}
}

// ---------------------------------------------------------------------------
// What the request is allowed to say
// ---------------------------------------------------------------------------

func TestTheRequestsOwnFieldsAreRefusedBeforeTheWorldIsTouched(t *testing.T) {
	t.Parallel()

	refusals := map[string]protocol.BlockEditRequest{
		// An absent action decodes as Unknown, and guessing which of the two a silent
		// client meant would be the server deciding an outcome from nothing.
		"an unknown action": {Pos: [3]int32{4, 200, 0}, HasPos: true, Action: vnet.EditActionUnknown},
		"an action outside the enum": {
			Pos: [3]int32{4, 200, 0}, HasPos: true, Action: vnet.EditAction(99),
		},
		// Slot bounds are announced by the server and checked before any lookup.
		"a slot outside the inventory": {
			Pos: [3]int32{4, 200, 0}, HasPos: true, Action: vnet.EditActionPlace, Slot: protocol.InventorySlots,
		},
	}

	for name, req := range refusals {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, chunks := editWorld(t)
			player, _ := h.join(1, [3]float32{0.5, 200, 0.5})

			before := chunks.Len()
			if _, err := player.Edit(context.Background(), req); err == nil {
				t.Fatalf("%s was accepted", name)
			}
			if after := chunks.Len(); after != before {
				t.Errorf("the cache grew from %d to %d chunks resolving a request that was malformed on its face", before, after)
			}
		})
	}
}

// An absent position must be refused rather than read as the origin. The player is close
// enough, owns a placeable block and the origin starts empty, so every rule after presence
// would accept the invented placement; the assertion cannot pass on a second refusal.
func TestAnAbsentPositionIsRefusedRatherThanReadAsTheOrigin(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 1, 0.5})
	giveBlock(t, h, player, chunks, world.Stone)
	if err := chunks.Apply(context.Background(), 0, 0, 0, world.Air, nil); err != nil {
		t.Fatalf("prepare empty origin: %v", err)
	}

	req := placeAt(t, player, [3]int32{}, world.Stone)
	req.HasPos = false
	if _, err := player.Edit(context.Background(), req); err == nil {
		t.Fatal("a request with no position was accepted")
	}
	if got := blockAt(t, chunks, 0, 0, 0); got != world.Air {
		t.Errorf("the voxel at the world origin became block %d from a request that named no position", got)
	}
}

// The numeric enum value remains decodable for wire compatibility, but it is no
// longer a legal edit. Mining is the only client path that may produce Air.
func TestADirectBreakIsWithdrawn(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	target := [3]int32{3, 200, 0}
	if err := chunks.Apply(context.Background(), 3, 200, 0, world.Stone, nil); err != nil {
		t.Fatalf("prepare Stone: %v", err)
	}

	_, err := player.Edit(context.Background(), protocol.BlockEditRequest{
		Pos: target, HasPos: true, Action: vnet.EditActionBreak, Slot: protocol.InventorySlots - 1,
	})
	if !errors.Is(err, game.ErrBreakActionWithdrawn) {
		t.Fatalf("direct Break returned %v, want ErrBreakActionWithdrawn", err)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Stone {
		t.Errorf("the retired direct break changed the target to block %d", got)
	}
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

// The break itself hands the player nothing. That is the whole of this issue: what a
// block yields becomes something lying in the world, and the pack changes only once
// somebody has walked over it.
func TestBreakingPutsNothingInThePackAndLeavesTheYieldOnTheGround(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	// The shelf sits on a chunk border, and since #717 a water voxel with an unread
	// neighbour is deferred rather than decided from fabricated blocks. This test is
	// about the yield, not the residency frontier, so the chunks the neighbourhood
	// crosses into are made resident. Composing the player's own chunk turns the
	// unread-is-solid ground under their feet into generated air, so a real block is
	// put back where they stand — and the shelf drops one level below the feet, so
	// that same block is what the freed water spreads against instead of flooding the
	// collector's cell and ferrying them out of pickup range on its current.
	for _, coord := range []world.Coord{{Y: 6, Z: -1}, {X: -1, Y: 6, Z: -1}, {Y: 6}} {
		if _, _, err := chunks.Get(context.Background(), coord); err != nil {
			t.Fatalf("compose bordering chunk %+v: %v", coord, err)
		}
	}
	if err := chunks.Apply(context.Background(), 0, 199, 0, world.Stone, nil); err != nil {
		t.Fatalf("floor the player: %v", err)
	}
	target, floor := dropShelf(t, player)
	target[1]--
	floor[1]--
	if err := chunks.Apply(context.Background(), int64(floor[0]), int64(floor[1]), int64(floor[2]), world.Stone, nil); err != nil {
		t.Fatalf("prepare the shelf under the drop: %v", err)
	}
	if err := chunks.Apply(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]), world.Dirt, nil); err != nil {
		t.Fatalf("prepare Dirt: %v", err)
	}
	if err := chunks.Apply(context.Background(), int64(target[0]-1), int64(target[1]), int64(target[2]), world.Water, nil); err != nil {
		t.Fatalf("prepare source beside Dirt: %v", err)
	}

	result := mineAt(t, h, player, target)
	if result.Inventory != nil {
		t.Fatal("the break reported an inventory change; the yield belongs on the ground")
	}
	if got := countOf(player.InventoryState(), game.ItemDirt); got != 0 {
		t.Errorf("the miner holds %d Dirt on the tick the block broke, want 0", got)
	}
	if got := blockAt(t, chunks, int64(target[0]), int64(target[1]), int64(target[2])); got != world.Air {
		t.Errorf("the broken voxel holds block %d, want Air", got)
	}
	h.step()
	if got := blockAt(t, chunks, int64(target[0]), int64(target[1]), int64(target[2])); got != world.WaterFlow7 {
		t.Errorf("broken voxel after one tick = %d, want scheduled WaterFlow7", got)
	}

	// Standing beside it is enough: no key, no aim, and no request from the client.
	collected := false
	for range 200 {
		h.step()
		if countOf(player.InventoryState(), game.ItemDirt) == 1 {
			collected = true
			break
		}
	}
	if !collected {
		t.Errorf("Dirt count held by the player = %d, want the drop to have been collected",
			countOf(player.InventoryState(), game.ItemDirt))
	}
}

func TestPlacingConsumesExactlyOneSelectedBlock(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	giveBlock(t, h, player, chunks, world.Stone)

	result, err := player.Edit(context.Background(), placeAt(t, player, [3]int32{3, 200, 0}, world.Stone))
	if err != nil {
		t.Fatalf("place Stone: %v", err)
	}
	if result.Inventory == nil {
		t.Fatal("the accepted placement reported no inventory change")
	}
	if got := countOf(*result.Inventory, game.ItemStone); got != 0 {
		t.Errorf("Stone count after placing = %d, want 0", got)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Stone {
		t.Errorf("the placed voxel holds block %d, want Stone", got)
	}
}

func TestRawIronCannotBePlacedAndChangesNeitherVoxelNorInventory(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	target := [3]int32{-3, 200, 0}
	giveBlock(t, h, player, chunks, world.IronOre)
	if got := countOf(player.InventoryState(), game.ItemRawIron); got != 1 {
		t.Fatalf("IronOre yielded %d RawIron, want 1", got)
	}
	before := protocol.EncodeInventoryState(player.InventoryState())

	if result, err := player.Edit(context.Background(), placeFromSlot(target, 0)); err == nil {
		t.Fatalf("placing RawIron was accepted with result %+v", result)
	}
	if got := blockAt(t, chunks, -3, 200, 0); got != world.Air {
		t.Errorf("RawIron placement left block %d, want Air", got)
	}
	after := protocol.EncodeInventoryState(player.InventoryState())
	if !bytes.Equal(after, before) {
		t.Errorf("RawIron placement changed the inventory")
	}
}

func TestPlacingWithAnEmptyStackIsRefusedBeforeTheWorldIsTouched(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	target := [3]int32{3, 200, 0}

	before := chunks.Len()
	if _, err := player.Edit(context.Background(), placeFromSlot(target, 0)); err == nil {
		t.Fatal("a placement with no Stone to spend was accepted")
	}
	if after := chunks.Len(); after != before {
		t.Errorf("the cache grew from %d to %d for an unpaid placement", before, after)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Air {
		t.Errorf("the refused placement left block %d in the world", got)
	}
}

// A world that will not accept the write is a refusal like any other: logged, silent, and
// never an accepted edit reported to the caller.
func TestAnEditorFailureIsARefusal(t *testing.T) {
	t.Parallel()

	sim, err := game.NewSim(game.DefaultTickRate, 2, testWorldSeed, flatWorld{groundTop: 200}, refusingEditor{}, testEntityIDs(), discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	h := &harness{t: t, sim: sim}
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})

	result := make(chan error, 1)
	go func() {
		completion, nextErr := player.NextMining(context.Background())
		if nextErr != nil {
			result <- nextErr
			return
		}
		_, completeErr := player.CompleteMining(context.Background(), completion)
		result <- completeErr
	}()
	// Mine until the refusal arrives rather than for a fixed number of ticks. This test is
	// about what a failing editor does, not about how long a block takes — and a tick count
	// written here is a hardness number restated in a file that has no business knowing one.
	// It was 20, which stopped being enough the moment #178 raised the table.
	const budget = 400
	var refusal error
	done := false
	for tick := uint64(1); tick <= budget && !done; tick++ {
		if err := player.Mine(protocol.MineRequest{
			Pos: [3]int32{3, 200, 0}, HasPos: true, Active: true, ClientTick: uint32(tick),
		}, true); err != nil {
			// The target has paid its cost and the write is in flight, which is the state
			// this loop exists to reach. Stop asking and wait for what the editor said.
			break
		}
		sim.Step(tick)
		select {
		case refusal = <-result:
			done = true
		default:
		}
	}
	if !done {
		select {
		case refusal = <-result:
		case <-time.After(2 * time.Second):
			t.Fatalf("mining did not reach the editor in %d ticks", budget)
		}
	}
	if !errors.Is(refusal, errEditorRefused) {
		t.Fatalf("CompleteMining returned %v, want the editor's own error", refusal)
	}
}

// ---------------------------------------------------------------------------
// Collision reads the edited world
// ---------------------------------------------------------------------------

// Digging a hole and falling into it. The point is not the physics — that has its own file
// — but that the terrain the tick loop collides against is the same composed chunk the edit
// changed. CacheTerrain remembers the chunk it last looked in, and a remembered chunk that
// never notices an edit is a player standing on ground that is no longer there.
func TestCollisionSeesAnEdit(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	spawn := openCountrySpawn(editSeed)
	generateAround(t, chunks, spawn, 1)

	player, _ := h.join(1, spawn)
	resting := h.settle(player)

	// The block holding the player up is the one under their feet, in the spawn's own
	// column.
	feet := int64(resting.Pos[1])
	under := [3]int32{int32(math.Floor(float64(spawn[0]))), int32(feet) - 1, int32(math.Floor(float64(spawn[2])))}
	if got := blockAt(t, chunks, int64(under[0]), int64(under[1]), int64(under[2])); got == world.Air {
		t.Fatalf("the player settled at y=%v with air under them", resting.Pos[1])
	}

	// Standing still on solid ground: without an edit, nothing moves them.
	h.advance(5)
	if got := player.State().Pos[1]; got != resting.Pos[1] {
		t.Fatalf("the player moved from %v to %v with no edit and no input", resting.Pos[1], got)
	}

	mineAt(t, h, player, under)

	// One tick is enough to start the fall; a few make the landing unambiguous.
	h.advance(20)
	if got := player.State().Pos[1]; got >= resting.Pos[1] {
		t.Fatalf("the player is still at y=%v after the ground under them was broken; collision is reading terrain from before the edit", got)
	}
}
