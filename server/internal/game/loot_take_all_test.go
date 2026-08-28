package game

import (
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Taking everything, and the entry the pack had no room for.
//
// Every test here is about the *walk*: which entries move, in what order, how many
// revisions that costs and what the character is told when the pack runs out halfway
// through. The containers are built rather than killed for, because a species table
// names its own drops and what is under test is a list of entries with a chosen shape —
// one that fits, one that cannot, one that fits behind the one that cannot.

const takeAllCorpse = 9

// standCorpse puts one owned, unexpired corpse in front of a joined character and opens
// it, so the take-all preconditions are all satisfied but the walk.
func standCorpse(t *testing.T, h *vitalsHarness, p *Player, stacks ...inventoryStack) {
	t.Helper()

	entries := make([]corpseEntry, len(stacks))
	for index, stack := range stacks {
		entries[index] = corpseEntry{entryID: uint64(index + 1), stack: stack}
	}
	h.sim.mu.Lock()
	h.sim.corpses[takeAllCorpse] = &corpse{
		entityID: takeAllCorpse, kind: vnet.MobKindDraugr, pos: [3]float64{0.5, 64, 0.5},
		chunk: p.chunk, owner: p.corpseOwner(), expiresTick: h.sim.corpseLifetimeTicks,
		container: corpseContainer{revision: 1, entries: entries},
	}
	h.sim.mu.Unlock()
	if reason, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: takeAllCorpse, ClientTick: 1}); err != nil {
		t.Fatalf("open = %s, %v", reason, err)
	}
}

// packWithOneEmptySlot leaves exactly `empty` general slots free and every other one
// holding a blade, which is the only shape that separates "does not fit" from "the pack
// is untouched": a durable item needs a whole empty slot, and a resource can still merge
// into a partial stack after one.
func packWithEmptySlots(t *testing.T, p *Player, empty int) {
	t.Helper()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	for slot := range p.inventory.slots[:equipmentFirst] {
		p.inventory.slots[slot] = stackOf(ItemRustySword, 1)
	}
	for slot := range empty {
		p.inventory.slots[slot] = inventoryStack{}
	}
}

func packSlots(p *Player) slotTable {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.inventory.slots
}

// containerNow is the authoritative revision and remaining entry ids, read under the
// lock that owns them.
func containerNow(t *testing.T, h *vitalsHarness, p *Player) (uint32, []uint64, bool) {
	t.Helper()

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	c := h.sim.corpses[takeAllCorpse]
	if c == nil {
		return 0, nil, false
	}
	container, owned := c.containerFor(p)
	if !owned {
		t.Fatalf("corpse %d does not belong to the character asking about it", takeAllCorpse)
	}
	ids := make([]uint64, len(container.entries))
	for index, entry := range container.entries {
		ids[index] = entry.entryID
	}
	return container.revision, ids, true
}

// lootFrames is every complete loot answer this session was sent, in delivery order:
// the states as (revision, entry ids) and the closures as corpse ids.
func lootFrames(t *testing.T, out *dropSink) (states [][]uint64, revisions []uint32, closed []uint64) {
	t.Helper()

	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		var payload flatbuffers.Table
		switch envelope.PayloadType() {
		case vnet.PayloadLootState:
			if !envelope.Payload(&payload) {
				t.Fatal("LootState envelope has no payload")
			}
			var state vnet.LootState
			state.Init(payload.Bytes, payload.Pos)
			ids := make([]uint64, 0, state.EntriesLength())
			for index := range state.EntriesLength() {
				var entry vnet.LootEntry
				if !state.Entries(&entry, index) {
					t.Fatalf("loot entry %d is missing from a state that claims to hold it", index)
				}
				ids = append(ids, entry.EntryId())
			}
			states = append(states, ids)
			revisions = append(revisions, state.Revision())
		case vnet.PayloadLootClosed:
			if !envelope.Payload(&payload) {
				t.Fatal("LootClosed envelope has no payload")
			}
			var closure vnet.LootClosed
			closure.Init(payload.Bytes, payload.Pos)
			closed = append(closed, closure.CorpseId())
		}
	}
	return states, revisions, closed
}

// **The entry that does not fit is stepped over, not stopped at.**
//
// The whole reason take-all is a walk rather than a loop of takes: a blade the pack has
// no empty slot for sits between two bones, and both bones still come home. Aborting on
// the first refusal would leave the third entry behind for no reason a player could see,
// and one revision per moved entry would make the client's view stale mid-walk.
func TestTakeAllStepsOverWhatDoesNotFitAndSpendsOneRevision(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	standCorpse(t, h, player, stackOf(ItemBone, 1), stackOf(ItemRustySword, 1), stackOf(ItemBone, 1))
	packWithEmptySlots(t, player, 1)

	reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 2})
	if err == nil || reason != vnet.RefusalReasonInventoryFull {
		t.Fatalf("partial take-all = %s, %v; want an InventoryFull refusal beside the entries that moved", reason, err)
	}

	revision, remaining, exists := containerNow(t, h, player)
	if !exists {
		t.Fatal("a corpse that still holds an entry was removed")
	}
	if revision != 2 {
		t.Errorf("revision = %d, want exactly one spent for the whole walk", revision)
	}
	if len(remaining) != 1 || remaining[0] != 2 {
		t.Errorf("remaining entries = %v, want only the blade at entry 2", remaining)
	}
	if got := heldCount(player.InventoryState(), ItemBone); got != 2 {
		t.Errorf("the pack holds %d Bone, want the 2 that fitted around the blade", got)
	}

	h.step()
	states, revisions, closed := lootFrames(t, out)
	if len(closed) != 0 {
		t.Errorf("a corpse that still holds loot was closed: %v", closed)
	}
	// One state, not two: the open and the walk both landed before the first tick, and
	// `lootDirty` is a debt to be settled rather than a queue of every revision the
	// container passed through.
	if len(states) != 1 || len(revisions) != 1 {
		t.Fatalf("loot states = %v at revisions %v, want the one remainder", states, revisions)
	}
	if revisions[0] != 2 || len(states[0]) != 1 || states[0][0] != 2 {
		t.Errorf("remainder state = revision %d entries %v, want revision 2 holding entry 2", revisions[0], states[0])
	}
}

// The bare corpse closes the window, and the closure is the existing one.
func TestTakeAllThatEmptiesTheCorpseClosesTheWindow(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	standCorpse(t, h, player, stackOf(ItemBone, 2), stackOf(ItemVargrPelt, 1))

	if reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 2}); err != nil {
		t.Fatalf("take-all = %s, %v", reason, err)
	}
	if _, _, exists := containerNow(t, h, player); exists {
		t.Error("the emptied corpse was not removed")
	}
	if got := heldCount(player.InventoryState(), ItemBone); got != 2 {
		t.Errorf("the pack holds %d Bone, want 2", got)
	}
	if got := heldCount(player.InventoryState(), ItemVargrPelt); got != 1 {
		t.Errorf("the pack holds %d VargrPelt, want 1", got)
	}

	h.step()
	states, revisions, closed := lootFrames(t, out)
	if len(closed) != 1 || closed[0] != takeAllCorpse {
		t.Errorf("closures = %v, want exactly the emptied corpse", closed)
	}
	if len(states) != 0 || len(revisions) != 0 {
		t.Errorf("loot states = %v at revisions %v; an emptied corpse owes a closure and nothing else", states, revisions)
	}
}

// Nothing fits: the container is exactly as it was, and no revision was spent on saying so.
func TestTakeAllThatFitsNothingSpendsNoRevision(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	standCorpse(t, h, player, stackOf(ItemBone, 1), stackOf(ItemVargrPelt, 1))
	packWithEmptySlots(t, player, 0)
	before := packSlots(player)

	reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 2})
	if err == nil || reason != vnet.RefusalReasonInventoryFull {
		t.Fatalf("full-pack take-all = %s, %v; want InventoryFull", reason, err)
	}
	revision, remaining, exists := containerNow(t, h, player)
	if !exists || revision != 1 || len(remaining) != 2 {
		t.Errorf("container = revision %d entries %v exists %v; want the untouched revision 1 with both entries", revision, remaining, exists)
	}
	if packSlots(player) != before {
		t.Error("a take-all that moved nothing still changed the pack")
	}
}

// The preconditions, one refusal each. Every one of them leaves the container alone.
func TestTakeAllRefusesTheSameThingsTakeLootDoes(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	standCorpse(t, h, player, stackOf(ItemBone, 1))

	if reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 7, ClientTick: 2}); err == nil || reason != vnet.RefusalReasonStaleRevision {
		t.Errorf("stale-revision take-all = %s, %v; want StaleRevision", reason, err)
	}
	if reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse + 1, Revision: 1, ClientTick: 3}); err == nil || reason != vnet.RefusalReasonCorpseUnavailable {
		t.Errorf("unknown-corpse take-all = %s, %v; want CorpseUnavailable", reason, err)
	}

	player.inventory.mu.Lock()
	reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 4})
	player.inventory.mu.Unlock()
	if err == nil || reason != vnet.RefusalReasonInventoryBusy {
		t.Errorf("busy-pack take-all = %s, %v; want InventoryBusy", reason, err)
	}

	// A repeat of a client tick already answered is silence rather than a refusal: it is
	// a duplicate of an intent the player expressed once, and there is nothing to tell
	// them about it.
	if reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 4}); err == nil || reason != vnet.RefusalReasonUnknown {
		t.Errorf("replayed take-all = %s, %v; want a silent refusal", reason, err)
	}

	if revision, remaining, exists := containerNow(t, h, player); !exists || revision != 1 || len(remaining) != 1 {
		t.Errorf("container after four refusals = revision %d entries %v exists %v", revision, remaining, exists)
	}

	h.hurt(player, PlayerMaxHealth)
	if reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 5}); err == nil || reason != vnet.RefusalReasonPlayerIsDead {
		t.Errorf("dead take-all = %s, %v; want PlayerIsDead", reason, err)
	}
}

// **Take-all is its own client-ordering stream.**
//
// Pressing F and clicking an entry are two intents, and a tick number spent on one must
// not silence the other. Open, take and take-all each carry their own newest tick for the
// reason attack and mining do.
func TestTakeAllOrderingDoesNotSilenceASingleTake(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	standCorpse(t, h, player, stackOf(ItemBone, 1), stackOf(ItemVargrPelt, 1))

	if reason, err := player.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 9}); err != nil {
		t.Fatalf("take-all = %s, %v", reason, err)
	}
	// The same client tick, on the other stream, after the corpse has already gone.
	if reason, err := player.TakeLoot(protocol.LootTakeRequest{CorpseID: takeAllCorpse, EntryID: 1, Revision: 2, ClientTick: 9}); err == nil || reason != vnet.RefusalReasonCorpseUnavailable {
		t.Fatalf("take on the same tick = %s, %v; want the corpse's own answer rather than a stale-tick silence", reason, err)
	}
}

// **A boss corpse is several containers, and take-all walks exactly one of them.**
//
// The one place where "take everything" could mean somebody else's everything.
// `containerFor` is what keeps it from meaning that, and this pins the walk to it rather
// than to the entry list `hasLoot` happens to see.
func TestTakeAllWalksOnlyTheRequestersOwnContainer(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	mine, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	theirs, _ := h.join(2, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	h.sim.corpses[takeAllCorpse] = &corpse{
		entityID: takeAllCorpse, kind: vnet.MobKindDraugr, pos: [3]float64{0.5, 64, 0.5},
		chunk: mine.chunk, expiresTick: h.sim.corpseLifetimeTicks,
		personal: map[corpseOwner]*corpseContainer{
			mine.corpseOwner(): {revision: 1, entries: []corpseEntry{
				{entryID: 1, stack: stackOf(ItemBone, 1)},
			}},
			theirs.corpseOwner(): {revision: 1, entries: []corpseEntry{
				{entryID: 1, stack: stackOf(ItemVargrPelt, 3)},
				{entryID: 2, stack: stackOf(ItemBone, 2)},
			}},
		},
	}
	h.sim.mu.Unlock()
	if reason, err := mine.OpenLoot(protocol.LootOpenRequest{CorpseID: takeAllCorpse, ClientTick: 1}); err != nil {
		t.Fatalf("open = %s, %v", reason, err)
	}
	if reason, err := mine.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: takeAllCorpse, Revision: 1, ClientTick: 2}); err != nil {
		t.Fatalf("take-all = %s, %v", reason, err)
	}

	revision, remaining, exists := containerNow(t, h, theirs)
	if !exists {
		t.Fatal("emptying one personal container removed another character's loot")
	}
	if revision != 1 || len(remaining) != 2 {
		t.Errorf("the other character's container = revision %d entries %v; want an untouched revision 1 with both entries", revision, remaining)
	}
	if got := heldCount(mine.InventoryState(), ItemVargrPelt); got != 0 {
		t.Errorf("the walk took %d VargrPelt out of a container it does not own", got)
	}
	if got := heldCount(mine.InventoryState(), ItemBone); got != 1 {
		t.Errorf("the requester holds %d Bone, want the 1 in their own container", got)
	}
}
