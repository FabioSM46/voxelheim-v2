package game

import (
	"context"
	"encoding/binary"
	"errors"
	"io"
	"log/slog"
	"math"
	"sync"
	"sync/atomic"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// testEntityIDs is the identity source session.Registry provides in production: one
// counter shared by every entity, so no id ever names two things.
//
// It starts well above the identities these tests hand to Join, because those are
// chosen by hand — a counter starting at 1 would mint a drop called 1 beside a player
// called 1 and make an assertion ambiguous for a reason that has nothing to do with
// the code under test.
func testEntityIDs() func() uint64 {
	var next atomic.Uint64
	next.Store(100)
	return func() uint64 { return next.Add(1) }
}

// testPlayerID is a distinct identity per entity id.
//
// The simulation keys players by entity id and never by identity, so these only have
// to differ from one another and to be non-zero — Join refuses the zero id, which is
// the digest of nothing and names nobody. Derived from the entity id rather than
// derived so that a failing test names the same player on every run.
func testPlayerID(entityID uint64) identity.PlayerID {
	var account identity.Account
	binary.LittleEndian.PutUint64(account[:8], entityID)
	return identity.IDOf(account)
}

// testAppearance is a character the contract would accept: every colour inside
// 0x00RRGGBB and a hair model that is a real member.
//
// Join validates what it is handed — a stored appearance is on its way to a
// PlayerAppearance every viewer may refuse — so a test that wants a player at all has
// to state a legal one. That is the point of asking there rather than trusting the
// caller, and it is why this is one helper instead of a literal per call.
func testAppearance() protocol.Appearance {
	return protocol.Appearance{
		SkinColor:     0x8d5524,
		ShirtColor:    0x2f4f4f,
		TrousersColor: 0x3b2f2f,
		ShoesColor:    0x1c1c1c,
		HairModel:     vnet.HairModelBraided,
		HairColor:     0xd8b46a,
	}
}

const testCharacterName = "Test Character"

// dropTerrain is solid at and below groundTop and air above it, with an optional
// region the server has not generated yet.
//
// absent is what makes this more than a flat world: the tick reads terrain through
// Peek, so a chunk streaming has not produced answers "not resident", and the rule
// under test is that such a voxel is solid rather than air.
type dropTerrain struct {
	groundTop int64
	absent    func(x, y, z int64) bool
}

func (w dropTerrain) Block(x, y, z int64) (world.Block, bool) {
	if w.absent != nil && w.absent(x, y, z) {
		return world.Air, false
	}
	if y <= w.groundTop {
		return world.Stone, true
	}
	return world.Air, true
}

func (w dropTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// refusedEdits is an Editor that writes nothing. The drop tests that spawn directly
// never edit the world, and NewSim refuses a nil editor — so this is how they say so.
type refusedEdits struct{}

func (refusedEdits) ApplyGuarded(context.Context, int64, int64, int64, world.Block, func() error, func(world.Block) error) error {
	return errors.New("this editor refuses every edit")
}

// dropHarness drives a simulation one tick at a time, the way the loop does.
type dropHarness struct {
	t    *testing.T
	sim  *Sim
	tick uint64
}

func newDropHarness(t *testing.T, terrain Terrain) *dropHarness {
	t.Helper()
	return newDropHarnessAt(t, terrain, 8)
}

func newDropHarnessAt(t *testing.T, terrain Terrain, viewDistance uint8) *dropHarness {
	t.Helper()
	return newDropHarnessAtTickRate(t, terrain, viewDistance, DefaultTickRate)
}

func newDropHarnessAtTickRate(t *testing.T, terrain Terrain, viewDistance, tickRate uint8) *dropHarness {
	t.Helper()

	sim, err := NewSim(tickRate, viewDistance, testWorldSeed, terrain, refusedEdits{}, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	return &dropHarness{t: t, sim: sim}
}

func (h *dropHarness) step() {
	h.tick++
	h.sim.Step(h.tick)
}

func (h *dropHarness) advance(n int) {
	h.t.Helper()
	for range n {
		h.step()
	}
}

// join admits a player and returns it with the queue its session would read.
func (h *dropHarness) join(entityID uint64, pos [3]float32) (*Player, *dropSink) {
	h.t.Helper()

	out := &dropSink{}
	player, err := h.sim.Join(entityID, testPlayerID(entityID), testCharacterName, pos, testAppearance(), nil, out.deliver)
	if err != nil {
		h.t.Fatalf("Join: %v", err)
	}
	return player, out
}

// spawn puts one drop in the world and returns the simulation's own handle on it, so
// an assertion can name a position rather than only what the wire carries.
func (h *dropHarness) spawn(item ItemID, count uint16, voxel [3]int64) *itemDrop {
	h.t.Helper()

	id, ok := h.sim.spawnDrop(item, count, voxel)
	if !ok {
		h.t.Fatalf("spawning %d of item %d at %v was refused", count, item, voxel)
	}
	return h.drop(id)
}

func (h *dropHarness) spawnStack(stack inventoryStack, voxel [3]int64) *itemDrop {
	h.t.Helper()

	id, ok := h.sim.spawnStackDrop(stack, voxel)
	if !ok {
		h.t.Fatalf("spawning stack %+v at %v was refused", stack, voxel)
	}
	return h.drop(id)
}

// drop is the live drop with this identity, or nil once it has stopped existing.
func (h *dropHarness) drop(id uint64) *itemDrop {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.sim.drops[id]
}

func (h *dropHarness) dropCount() int {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return len(h.sim.drops)
}

// dropSink stands in for a session's outbound queue. Guarded, because Step delivers
// from the tick goroutine while a test reads from its own.
type dropSink struct {
	mu     sync.Mutex
	frames [][]byte
	full   bool
}

func (s *dropSink) deliver(frame []byte) bool {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.full {
		return false
	}
	s.frames = append(s.frames, frame)
	return true
}

func (s *dropSink) setFull(full bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.full = full
}

func (s *dropSink) all() [][]byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([][]byte(nil), s.frames...)
}

// snapshotDrops is the drop vector of the newest EntitySnapshot this session was sent.
func (s *dropSink) snapshotDrops(t *testing.T) []protocol.ItemDropState {
	t.Helper()

	var newest []protocol.ItemDropState
	found := false
	for _, frame := range s.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadEntitySnapshot {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("EntitySnapshot envelope has no payload")
		}
		var snapshot vnet.EntitySnapshot
		snapshot.Init(payload.Bytes, payload.Pos)

		found = true
		newest = newest[:0]
		byID := make(map[uint64]int, snapshot.DropsLength())
		for i := range snapshot.DropsLength() {
			var drop vnet.ItemDropState
			if !snapshot.Drops(&drop, i) {
				t.Fatalf("drop %d is missing from a snapshot that claims to hold it", i)
			}
			pos := drop.Pos(nil)
			if pos == nil {
				t.Fatalf("drop %d carries no position", i)
			}
			byID[drop.EntityId()] = len(newest)
			newest = append(newest, protocol.ItemDropState{
				EntityID: drop.EntityId(),
				Pos:      [3]float32{pos.X(), pos.Y(), pos.Z()},
				ItemID:   drop.ItemId(),
				Count:    drop.Count(),
			})
		}
		for i := range snapshot.DropDurabilitiesLength() {
			var wear vnet.ItemDropDurability
			if !snapshot.DropDurabilities(&wear, i) {
				t.Fatalf("drop durability %d is missing from a snapshot that claims to hold it", i)
			}
			index, ok := byID[wear.EntityId()]
			if !ok {
				t.Fatalf("drop durability %d names unknown drop %d", i, wear.EntityId())
			}
			newest[index].Durability = wear.Durability()
			newest[index].MaxDurability = wear.MaxDurability()
		}
	}
	if !found {
		t.Fatal("the session received no snapshot at all")
	}
	return newest
}

// inventoryStates is every complete authoritative inventory this session was sent.
func (s *dropSink) inventoryStates(t *testing.T) []protocol.InventoryState {
	t.Helper()

	var states []protocol.InventoryState
	for _, frame := range s.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadInventoryState {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("InventoryState envelope has no payload")
		}
		var table vnet.InventoryState
		table.Init(payload.Bytes, payload.Pos)

		state := protocol.InventoryState{Stacks: make([]protocol.InventoryStack, 0, table.StacksLength()/2)}
		for index := 0; index+1 < table.StacksLength(); index += 2 {
			// Durability rides in two vectors parallel to the stack pairs: slot i is
			// pairs 2i/2i+1 and durability entry i. Read rather than ignored, because a
			// projection that paired one slot's count with another's wear would encode
			// and decode perfectly, and this is one of the two places on this side that
			// would see it.
			slot := index / 2
			stack := protocol.InventoryStack{
				ItemID: table.Stacks(index),
				Count:  table.Stacks(index + 1),
			}
			if slot < table.DurabilityLength() {
				stack.Durability = table.Durability(slot)
			}
			if slot < table.MaxDurabilityLength() {
				stack.MaxDurability = table.MaxDurability(slot)
			}
			state.Stacks = append(state.Stacks, stack)
		}
		states = append(states, state)
	}
	return states
}

func heldCount(state protocol.InventoryState, item ItemID) uint16 {
	total := uint16(0)
	for _, stack := range state.Stacks {
		if stack.ItemID == uint16(item) {
			total += stack.Count
		}
	}
	return total
}

// dropTolerance is how close a position has to be for these tests to call it exact.
// Collision stops a hair short of the face it hit, so literal equality would be
// asserting the size of collisionSkin rather than the behaviour.
const dropTolerance = 1e-3

// ---------------------------------------------------------------------------
// Falling
// ---------------------------------------------------------------------------

// A drop is not a number attached to a voxel: it falls with the same integrator and
// the same collision the player uses, and it stops on the first surface under it.
func TestADropFallsAndComesToRestOnTheSurfaceBelowIt(t *testing.T) {
	t.Parallel()

	terrain := dropTerrain{groundTop: 63}
	h := newDropHarness(t, terrain)
	drop := h.spawn(ItemStone, 1, [3]int64{0, 80, 0})

	for range 200 {
		h.step()
		if overlaps(terrain, drop.box()) {
			t.Fatalf("the drop's box is inside a solid at y=%v", drop.pos[1])
		}
	}

	// The top face of the surface is at groundTop+1, and a drop's position is the
	// bottom of its box, exactly as a player's is the bottom of theirs.
	if got := drop.pos[1]; math.Abs(got-64) > dropTolerance {
		t.Errorf("the drop came to rest at y=%v, want the surface at y=64", got)
	}
	if drop.fallSpeed != 0 {
		t.Errorf("the resting drop still carries %v blocks/s of fall speed", drop.fallSpeed)
	}

	// And it stays there. A drop that keeps sinking a hair per tick is a drop that is
	// inside the floor a minute later.
	resting := drop.pos
	h.advance(100)
	if drop.pos != resting {
		t.Errorf("a resting drop moved from %v to %v with nothing under it changing", resting, drop.pos)
	}
}

// Three blocks a tick at terminal velocity: without the sub-stepping the collision
// does for the player, a long fall would step straight over a one-block floor.
func TestALongFallDoesNotPassThroughTheFloor(t *testing.T) {
	t.Parallel()

	terrain := dropTerrain{groundTop: 0}
	h := newDropHarness(t, terrain)
	drop := h.spawn(ItemStone, 1, [3]int64{0, 400, 0})

	h.advance(400)
	if got := drop.pos[1]; math.Abs(got-1) > dropTolerance {
		t.Errorf("the drop is at y=%v after a long fall, want it resting on the floor at y=1", got)
	}
}

// The same rule the tick already follows for every other terrain read: a chunk that is
// not resident is solid, so a drop over one waits instead of falling out of a world
// that is merely still loading — and waits with no accumulated speed, so it does not
// arrive with three seconds of fall in it when the chunk lands.
func TestADropOverATerrainMissHoldsWhereItIs(t *testing.T) {
	t.Parallel()

	// Everything below y=64 is still being streamed.
	h := newDropHarness(t, dropTerrain{groundTop: 63, absent: func(_, y, _ int64) bool { return y < 64 }})
	drop := h.spawn(ItemStone, 1, [3]int64{0, 64, 0})

	h.advance(100)
	if got := drop.pos[1]; math.Abs(got-64) > dropTolerance {
		t.Errorf("the drop is at y=%v; it should be held up by the chunk that has not arrived", got)
	}
	if drop.fallSpeed != 0 {
		t.Errorf("the held drop accumulated %v blocks/s of fall speed while it waited", drop.fallSpeed)
	}
}

// ---------------------------------------------------------------------------
// Where a break puts it
// ---------------------------------------------------------------------------

func TestBreakingSpawnsOneDropAtTheCentreOfTheVoxel(t *testing.T) {
	t.Parallel()

	sim, player, w, _ := newMiningPlayer(t, nil)
	target := [3]int32{3, 200, 0}
	w.set(target, world.CoalOre)

	if _, err := player.breakMined(context.Background(), target, world.CoalOre); err != nil {
		t.Fatalf("breakMined: %v", err)
	}

	sim.mu.Lock()
	defer sim.mu.Unlock()
	if len(sim.drops) != 1 {
		t.Fatalf("the break left %d drops in the world, want 1", len(sim.drops))
	}
	for _, drop := range sim.drops {
		if drop.item != ItemRawCoal || drop.count != 1 {
			t.Errorf("the drop carries %d of item %d, want one RawCoal", drop.count, drop.item)
		}
		if drop.durability != 0 || drop.maxDurability != 0 {
			t.Errorf("the block yield carries durability %d/%d, want a wearless world drop", drop.durability, drop.maxDurability)
		}
		// The wire position is the centre of the voxel that was broken, because that is
		// where the client draws a cube centred on it.
		want := [3]float32{3.5, 200.5, 0.5}
		if got := drop.wirePos(); got != want {
			t.Errorf("the drop is sent at %v, want the centre of the broken voxel %v", got, want)
		}
	}
}

// A throw belongs only to Player.DropItem. A yield produced by the world keeps the
// exact x/z chosen by that world event while gravity settles it vertically.
func TestAWorldProducedDropHasNoHorizontalMotion(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	drop := h.spawn(ItemStone, 1, [3]int64{4, 70, -3})
	startX, startZ := drop.pos[0], drop.pos[2]
	h.advance(100)

	if drop.pos[0] != startX || drop.pos[2] != startZ {
		t.Errorf("the world drop moved horizontally from (%v, %v) to (%v, %v)", startX, startZ, drop.pos[0], drop.pos[2])
	}
}

// Leaves are the drop table's explicit "nothing", and an unlisted block is its
// implicit one. Neither may cost an entity or an identity.
func TestABlockThatYieldsNothingSpawnsNoDrop(t *testing.T) {
	t.Parallel()

	sim, player, w, _ := newMiningPlayer(t, nil)
	target := [3]int32{3, 200, 0}
	w.set(target, world.Leaves)

	if _, err := player.breakMined(context.Background(), target, world.Leaves); err != nil {
		t.Fatalf("breakMined: %v", err)
	}
	sim.mu.Lock()
	defer sim.mu.Unlock()
	if len(sim.drops) != 0 {
		t.Errorf("breaking Leaves left %d drops in the world", len(sim.drops))
	}
}

// One counter for every entity: a drop can never be handed an identity a player is
// already using, because there is no second place identities come from.
func TestDropIdentitiesComeFromTheCounterThatNamesPlayers(t *testing.T) {
	t.Parallel()

	mint := testEntityIDs()
	sim, err := NewSim(DefaultTickRate, 2, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{}, mint,
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	// A session mints its player's entity id from the same source before it joins.
	entityID := mint()
	if _, err := sim.Join(entityID, testPlayerID(entityID), testCharacterName, [3]float32{0.5, 64, 0.5}, testAppearance(), nil, func([]byte) bool { return true }); err != nil {
		t.Fatalf("Join: %v", err)
	}

	first, ok := sim.spawnDrop(ItemStone, 1, [3]int64{0, 70, 0})
	if !ok {
		t.Fatal("the first drop was refused")
	}
	second, ok := sim.spawnDrop(ItemStone, 1, [3]int64{4, 70, 0})
	if !ok {
		t.Fatal("the second drop was refused")
	}

	if first == entityID || second == entityID || first == second {
		t.Errorf("ids collide: player %d, drops %d and %d", entityID, first, second)
	}
	if first <= entityID || second <= first {
		t.Errorf("ids are not monotonic: player %d, then %d, then %d", entityID, first, second)
	}
}

// ---------------------------------------------------------------------------
// Collecting
// ---------------------------------------------------------------------------

// The delay is what makes a drop something a player sees before they have it. Without
// it, breaking a block at your feet is indistinguishable from the inventory insert this
// issue replaced.
func TestAFreshDropIsCollectedOnTheEleventhTickAndNotBefore(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	drop := h.spawn(ItemStone, 1, [3]int64{1, 64, 0})

	for tick := 1; tick <= dropPickupDelayTicks; tick++ {
		h.step()
		if got := heldCount(player.InventoryState(), ItemStone); got != 0 {
			t.Fatalf("the drop was collected on tick %d, %d ticks before it may be", tick, dropPickupDelayTicks+1-tick)
		}
		if h.drop(drop.entityID) == nil {
			t.Fatalf("the drop stopped existing on tick %d", tick)
		}
	}

	h.step()
	if got := heldCount(player.InventoryState(), ItemStone); got != 1 {
		t.Errorf("the player holds %d Stone on the eleventh tick, want 1", got)
	}
	if h.drop(drop.entityID) != nil {
		t.Error("the collected drop is still lying in the world")
	}

	// Collecting is a real count change, so the whole authoritative inventory follows it.
	states := out.inventoryStates(t)
	if len(states) != 1 {
		t.Fatalf("the pickup produced %d inventory states, want exactly 1", len(states))
	}
	if got := heldCount(states[0], ItemStone); got != 1 {
		t.Errorf("the delivered inventory holds %d Stone, want 1", got)
	}
}

// A drop the player is nowhere near is not collected, however long it lies there.
func TestADropOutOfReachIsNotCollected(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	drop := h.spawn(ItemStone, 1, [3]int64{3, 64, 0})

	h.advance(100)
	if h.drop(drop.entityID) == nil {
		t.Fatal("a drop two blocks past the pickup radius was collected")
	}
	if got := heldCount(player.InventoryState(), ItemStone); got != 0 {
		t.Errorf("the player holds %d Stone from a drop out of reach", got)
	}
}

// A full pack is a reason to leave something on the ground, never a reason to destroy
// it. What fits is taken and the rest stays exactly where it was, with its count
// reduced by what was taken.
func TestAFullPackLeavesTheRemainderOnTheGround(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// Thirty-five full pack stacks and one with a single space in it: room for exactly
	// one. The trailing equipment slots stay empty and automatic pickup must ignore them.
	player.inventory.mu.Lock()
	for slot := range player.inventory.slots[:equipmentFirst] {
		player.inventory.slots[slot] = inventoryStack{item: ItemStone, count: 64}
	}
	player.inventory.slots[equipmentFirst-1].count = 63
	player.inventory.mu.Unlock()

	drop := h.spawn(ItemStone, 10, [3]int64{1, 64, 0})
	h.advance(dropPickupDelayTicks + 5)

	remaining := h.drop(drop.entityID)
	if remaining == nil {
		t.Fatal("the drop was consumed whole by a pack with room for one of it")
	}
	if remaining.count != 9 {
		t.Errorf("the drop on the ground holds %d, want the 9 that did not fit", remaining.count)
	}
	if got := heldCount(player.InventoryState(), ItemStone); got != 64*36 {
		t.Errorf("the player holds %d Stone, want a full pack of %d", got, 64*36)
	}
}

// A pack with no room at all changes nothing: the drop keeps its whole count and the
// client is told nothing, because nothing happened.
func TestAPackWithNoRoomLeavesTheDropUntouched(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	for slot := range player.inventory.slots {
		player.inventory.slots[slot] = inventoryStack{item: ItemDirt, count: 64}
	}
	player.inventory.mu.Unlock()

	drop := h.spawn(ItemStone, 4, [3]int64{1, 64, 0})
	h.advance(dropPickupDelayTicks + 5)

	remaining := h.drop(drop.entityID)
	if remaining == nil || remaining.count != 4 {
		t.Fatalf("the drop is %+v, want all 4 still on the ground", remaining)
	}
	if states := out.inventoryStates(t); len(states) != 0 {
		t.Errorf("a pickup that moved nothing sent %d inventory states", len(states))
	}
}

// An inventory state is not superseded by the next tick's, so a full queue may not
// lose one: the pickup keeps offering it until a tick gets it through.
func TestAPickupsInventoryStateIsRetriedUntilTheQueueTakesIt(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	out.setFull(true)

	h.spawn(ItemStone, 1, [3]int64{1, 64, 0})
	h.advance(dropPickupDelayTicks + 5)
	if got := heldCount(player.InventoryState(), ItemStone); got != 1 {
		t.Fatalf("the player holds %d Stone; the pickup itself must not depend on the queue", got)
	}
	if states := out.inventoryStates(t); len(states) != 0 {
		t.Fatalf("a full queue accepted %d frames", len(states))
	}

	out.setFull(false)
	h.step()
	states := out.inventoryStates(t)
	if len(states) != 1 {
		t.Fatalf("the deferred inventory state arrived %d times, want once", len(states))
	}
	if got := heldCount(states[0], ItemStone); got != 1 {
		t.Errorf("the retried state holds %d Stone, want 1", got)
	}

	// And it stops once it has been delivered.
	h.advance(5)
	if states := out.inventoryStates(t); len(states) != 1 {
		t.Errorf("the inventory state was sent %d times; the flag was never cleared", len(states))
	}
}

// The frame a pickup encodes is still current when it enters the queue.
//
// A session goroutine waiting for the inventory lock must not be able to spend a slot
// and enqueue its own newer state in the gap between this encode and this send — the
// client's last word about its own pack would then be a state older than the one it
// already had, with nothing left to resend it. Asserted from inside deliver, which the
// tick calls synchronously: a lock that cannot be taken there is a lock still held.
func TestAPickupHoldsTheInventoryLockAcrossItsDelivery(t *testing.T) {
	t.Parallel()

	sim, err := NewSim(DefaultTickRate, 8, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{}, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	var player *Player
	var heldDuringDelivery []bool
	deliver := func(frame []byte) bool {
		if vnet.GetRootAsEnvelope(frame, 0).PayloadType() != vnet.PayloadInventoryState {
			return true
		}
		free := player.inventory.mu.TryLock()
		if free {
			player.inventory.mu.Unlock()
		}
		heldDuringDelivery = append(heldDuringDelivery, !free)
		return true
	}

	// Assigned before any tick can run, and read on the tick goroutine, which in this
	// test is this one: Step calls deliver synchronously.
	admitted, err := sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 64, 0.5}, testAppearance(), nil, deliver)
	if err != nil {
		t.Fatalf("Join: %v", err)
	}
	player = admitted

	if _, ok := sim.spawnDrop(ItemStone, 1, [3]int64{1, 64, 0}); !ok {
		t.Fatal("the drop was refused")
	}
	for tick := uint64(1); tick <= dropPickupDelayTicks+5; tick++ {
		sim.Step(tick)
	}

	if len(heldDuringDelivery) != 1 {
		t.Fatalf("the pickup delivered %d inventory states, want exactly 1", len(heldDuringDelivery))
	}
	if !heldDuringDelivery[0] {
		t.Error("the inventory lock was free while the pickup's state was being queued")
	}
}

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

func TestTwoDropsOfOneItemMergeAndTwoOfDifferentItemsDoNot(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	first := h.spawn(ItemStone, 3, [3]int64{0, 64, 0})
	second := h.spawn(ItemStone, 4, [3]int64{1, 64, 0})
	other := h.spawn(ItemDirt, 2, [3]int64{0, 64, 1})

	h.step()

	if h.drop(second.entityID) != nil {
		t.Error("the younger of two identical drops is still in the world")
	}
	merged := h.drop(first.entityID)
	if merged == nil {
		t.Fatal("the older drop disappeared instead of absorbing the younger one")
	}
	if merged.count != 7 {
		t.Errorf("the merged drop holds %d, want 3 + 4", merged.count)
	}
	if kept := h.drop(other.entityID); kept == nil || kept.count != 2 {
		t.Errorf("the Dirt drop is %+v; different items must not merge", kept)
	}
}

func TestWornDropsWithDifferentDurabilityNeverMerge(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	first := h.spawnStack(inventoryStack{
		item: ItemRustySword, count: 1, durability: 12, maxDurability: RustySwordMaxDurability,
	}, [3]int64{0, 64, 0})
	second := h.spawnStack(inventoryStack{
		item: ItemRustySword, count: 1, durability: 80, maxDurability: RustySwordMaxDurability,
	}, [3]int64{1, 64, 0})

	h.step()

	if got := h.dropCount(); got != 2 {
		t.Fatalf("the two worn blades became %d drops, want two distinct objects", got)
	}
	if got := h.drop(first.entityID); got == nil || got.durability != 12 || got.count != 1 {
		t.Errorf("the first worn drop became %+v", got)
	}
	if got := h.drop(second.entityID); got == nil || got.durability != 80 || got.count != 1 {
		t.Errorf("the second worn drop became %+v", got)
	}
}

// Merging respects the item's own stack limit, and what does not fit stays where it
// was rather than vanishing into a stack that cannot hold it.
func TestMergingStopsAtTheStackLimit(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	limit := stackLimit(ItemStone)
	first := h.spawn(ItemStone, limit-4, [3]int64{0, 64, 0})
	second := h.spawn(ItemStone, 10, [3]int64{1, 64, 0})

	h.step()

	merged := h.drop(first.entityID)
	if merged == nil || merged.count != limit {
		t.Fatalf("the older drop is %+v, want it filled to the stack limit of %d", merged, limit)
	}
	leftover := h.drop(second.entityID)
	if leftover == nil || leftover.count != 6 {
		t.Fatalf("the younger drop is %+v, want the 6 that did not fit", leftover)
	}
}

// A merge must not renew a lifetime. The survivor is always the older drop, so a
// mining spree that keeps merging into one pile still despawns five minutes after its
// first block was broken.
func TestAMergeKeepsTheOlderDropsAge(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	first := h.spawn(ItemStone, 1, [3]int64{0, 64, 0})
	h.advance(50)

	second := h.spawn(ItemStone, 1, [3]int64{1, 64, 0})
	h.step()

	merged := h.drop(first.entityID)
	if merged == nil {
		t.Fatal("the older drop did not survive the merge")
	}
	if merged.age != 51 {
		t.Errorf("the merged drop's age is %d ticks, want the older drop's 51", merged.age)
	}
	if h.drop(second.entityID) != nil {
		t.Error("the younger drop survived a merge it was absorbed by")
	}
}

// ---------------------------------------------------------------------------
// Despawning
// ---------------------------------------------------------------------------

// Five minutes of simulated time, whether or not anybody is nearby. There is no player
// in this test at all, which is the point: a drop is simulation state and the world
// tidies itself.
func TestADropDespawnsAfterFiveMinutesWithNobodyPresent(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	drop := h.spawn(ItemStone, 1, [3]int64{0, 64, 0})

	lifetime := dropLifetimeTicks(DefaultTickRate)
	if lifetime != 5*60*DefaultTickRate {
		t.Fatalf("the lifetime is %d ticks, want five minutes at %d Hz", lifetime, DefaultTickRate)
	}

	h.advance(lifetime - 1)
	if h.drop(drop.entityID) == nil {
		t.Fatalf("the drop despawned before its %d ticks were up", lifetime)
	}

	h.step()
	if h.drop(drop.entityID) != nil {
		t.Errorf("the drop is still in the world after %d ticks", lifetime)
	}
	if h.dropCount() != 0 {
		t.Errorf("%d drops remain in the simulation", h.dropCount())
	}
}

// ---------------------------------------------------------------------------
// What a session is told
// ---------------------------------------------------------------------------

// The same cube a player entity is streamed by. A drop lying on a chunk this session
// holds is one it can draw; a drop beyond that cube would be an entity standing on
// terrain the client has never been sent.
func TestADropIsInTheSnapshotOnlyForSessionsThatCanSeeItsChunk(t *testing.T) {
	t.Parallel()

	// One chunk of view distance, so a drop two chunks away is unambiguously outside it.
	h := newDropHarnessAt(t, dropTerrain{groundTop: 63}, 1)
	_, near := h.join(1, [3]float32{0.5, 64, 0.5})
	_, far := h.join(2, [3]float32{float32(3*world.ChunkSize) + 0.5, 64, 0.5})

	// Beyond the pickup radius of the near player, and inside their view volume.
	drop := h.spawn(ItemStone, 2, [3]int64{5, 64, 0})
	h.advance(3)

	seen := near.snapshotDrops(t)
	if len(seen) != 1 {
		t.Fatalf("the session next to the drop sees %d drops, want 1", len(seen))
	}
	want := protocol.ItemDropState{
		EntityID: drop.entityID,
		Pos:      drop.wirePos(),
		ItemID:   uint16(ItemStone),
		Count:    2,
	}
	if seen[0] != want {
		t.Errorf("the snapshot carries %+v, want %+v", seen[0], want)
	}
	if got := far.snapshotDrops(t); len(got) != 0 {
		t.Errorf("a session three chunks away was sent %d drops", len(got))
	}
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

// Breaks resolve on session goroutines while the tick falls, merges and collects what
// they left. Worth running under -race: spawning takes the simulation's lock from
// outside it, and a pickup reaches into the inventory a session goroutine also writes.
func TestBreaksAndPickupsRunConcurrently(t *testing.T) {
	t.Parallel()

	blocks := make(map[[3]int64]world.Block)
	sim, player, w, _ := newMiningPlayer(t, blocks)

	const breaks = 40
	done := make(chan struct{})
	go func() {
		defer close(done)
		for i := range breaks {
			target := [3]int32{1, 200, int32(i)}
			w.set(target, world.Stone)
			if _, err := player.breakMined(context.Background(), target, world.Stone); err != nil {
				t.Errorf("breakMined at %v: %v", target, err)
				return
			}
		}
	}()

	for tick := uint64(1); ; tick++ {
		sim.Step(tick)
		select {
		case <-done:
			// A few more ticks so every drop has had its chance to be collected or to
			// settle; the assertion is only that nothing raced or was lost.
			for extra := tick + 1; extra <= tick+60; extra++ {
				sim.Step(extra)
			}
			sim.mu.Lock()
			onGround := 0
			for _, drop := range sim.drops {
				onGround += int(drop.count)
			}
			sim.mu.Unlock()
			carried := int(heldCount(player.InventoryState(), ItemStone))
			if onGround+carried != breaks {
				t.Errorf("%d items on the ground and %d carried, want %d in total", onGround, carried, breaks)
			}
			return
		default:
		}
	}
}
