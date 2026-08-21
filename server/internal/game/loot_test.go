package game

import (
	"io"
	"log/slog"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// What the dead leave behind, and what one of those things mends.
//
// Every assertion here is about what the *server* decided, and the ones about counts are
// exact rather than statistical — which is only possible because the generator is seeded
// from the world seed and advanced only inside the locked tick. A package-level rand or a
// reading of the wall clock passes nothing in this file.

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

// drops is every drop in the world, in identity order, copied under the lock that owns
// them. A copy rather than the pointers, because a test reads them on its own goroutine
// while the tick goroutine may still be stepping.
func (h *vitalsHarness) drops() []itemDrop {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	live := h.sim.sortedDropsLocked()
	seen := make([]itemDrop, len(live))
	for i, d := range live {
		seen[i] = *d
	}
	return seen
}

// killWithTheStarterBlade swings until the named creature is gone, and reports where it
// was standing on the tick it died.
//
// Through the authoritative path — Attack, then the tick — rather than by calling
// damageMobLocked, because the whole question this file asks is what happens when a kill
// is resolved *inside* the tick, under the lock spawnDrop wants.
func (h *vitalsHarness) killWithTheStarterBlade(p *Player, id uint64) [3]float64 {
	h.t.Helper()

	for blow := 1; blow <= 10; blow++ {
		stood, alive := h.mobState(id)
		if !alive {
			h.t.Fatalf("creature %d was already gone at blow %d", id, blow)
		}
		// The client tick is taken from the simulation's own rather than from the blow
		// number, because Attack refuses a stale one and a second kill in the same harness
		// would otherwise start counting again from 1.
		if err := h.swing(p, 0, uint32(h.tick)+1); err != nil {
			h.t.Fatalf("swing %d was refused: %v", blow, err)
		}
		h.step()
		if _, stillAlive := h.mobState(id); !stillAlive {
			return stood.pos
		}
		// Past the cooldown before the next one, which is the server's cadence and not
		// the client's.
		h.advance(int(h.sim.attackCooldown))
	}
	h.t.Fatalf("ten blows of the starter blade did not kill creature %d", id)
	return [3]float64{}
}

// standAt moves a player to an exact position, the way a test needs rather than the way
// the integrator would.
func (h *vitalsHarness) standAt(p *Player, pos [3]float64) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.pos = pos
	p.chunk = chunkAt(pos)
}

// armedAgainst is a player holding the starter blade with one creature of the named
// species standing in front of them, on flat ground.
func armedAgainst(t *testing.T, kind vnet.MobKind, at [3]float64) (*vitalsHarness, *Player, *dropSink, uint64) {
	t.Helper()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	return h, player, out, h.placeSpeciesAt(kind, at)
}

// ---------------------------------------------------------------------------
// The three items
// ---------------------------------------------------------------------------

// The ids, and the registry rows behind them.
//
// Pinned rather than derived, for the reason every id before them is: iota renumbers
// everything after an insertion, and the client mirrors these numbers to draw a pack. The
// campfire took 12, so these are 13, 14 and 15 and no other numbers will do.
func TestTheLootItemsCarryThePinnedIdsAndTheirOwnStats(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		item ItemID
		id   ItemID
		want itemDefinition
	}{
		{ItemBone, 13, itemDefinition{places: 0, maxStack: 64}},
		{ItemVargrPelt, 14, itemDefinition{places: 0, maxStack: 16}},
		{ItemLeatherPatch, 15, itemDefinition{places: 0, maxStack: 8, repairRestore: LeatherPatchRestore}},
	} {
		if tc.item != tc.id {
			t.Errorf("item %d was expected to be %d", tc.item, tc.id)
		}
		definition, registered := itemByID(tc.item)
		if !registered {
			t.Errorf("item %d is not registered", tc.item)
			continue
		}
		if definition != tc.want {
			t.Errorf("item %d is %+v, want %+v", tc.item, definition, tc.want)
		}
		// None of the three places a voxel. The zero `places` above says so; this is the
		// same claim asked of the palette, which has the final word on placeability.
		if block, placeable := blockPlacedBy(tc.item); placeable {
			t.Errorf("item %d places block %d", tc.item, block)
		}
	}
}

// Nothing consumes a bone, and that is a decision rather than an oversight.
//
// GDD §7's engraving table is what will want them; until it exists a bone is a resource
// with no sink, which is a resource. The claim is executed here rather than only written
// down in items.go, so the day a recipe does spend one, this test is where the comment
// gets corrected.
func TestNothingConsumesABoneYet(t *testing.T) {
	t.Parallel()

	for id, r := range recipeTable {
		for _, needed := range r.ingredients {
			if needed.item == ItemBone {
				t.Errorf("%s costs %d bones; items.go says nothing spends them yet", id, needed.count)
			}
		}
		if r.product == ItemBone {
			t.Errorf("%s produces bones, which come off a corpse rather than out of a recipe", id)
		}
	}
	// And a bone is not a weapon, a kit or a structure — the three things a non-zero
	// field in its row would quietly make it.
	bone, registered := itemByID(ItemBone)
	if !registered {
		t.Fatal("the bone is not registered")
	}
	if bone.meleeDamage != 0 || bone.repairRestore != 0 || bone.maxDurability != 0 {
		t.Errorf("the bone's row is %+v; it is a plain resource", bone)
	}
}

// ---------------------------------------------------------------------------
// What a kill leaves
// ---------------------------------------------------------------------------

// A draugr killed with the starter blade leaves its bones where it stood.
//
// The count is exact rather than "one or two": the roll comes from the simulation's own
// generator, seeded from the package's shared world seed, so the first kill in this world
// leaves exactly one bone and will do so on every run and every machine. That is the
// whole reason the generator is owned and locked the way it is.
func TestAKilledDraugrLeavesItsBonesWhereItStood(t *testing.T) {
	t.Parallel()

	h, player, _, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	stood := h.killWithTheStarterBlade(player, id)

	left := h.drops()
	if len(left) != 1 {
		t.Fatalf("a dead draugr left %d drops, want exactly one", len(left))
	}
	if left[0].item != ItemBone {
		t.Errorf("a dead draugr left item %d, want bones (%d)", left[0].item, ItemBone)
	}
	if left[0].count != 1 {
		t.Errorf("a dead draugr left %d bones; this world's first roll is exactly 1", left[0].count)
	}

	// At the mob's position, through the drop path a mined block already uses: the voxel
	// it was standing in, with the box centred in it.
	want := dropSpawnPos(voxelAt(stood))
	for axis := range 3 {
		if math.Abs(left[0].pos[axis]-want[axis]) > dropTolerance {
			t.Fatalf("the bones are at %v, want the draugr's own voxel at %v", left[0].pos, want)
		}
	}
}

// A vargr leaves exactly one pelt, and the fixed count is the balance: two pelts make one
// patch, so a vargr is half a repair whatever the generator says.
func TestAKilledVargrLeavesOnePelt(t *testing.T) {
	t.Parallel()

	h, player, _, id := armedAgainst(t, vnet.MobKindVargr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(player, id)

	left := h.drops()
	if len(left) != 1 {
		t.Fatalf("a dead vargr left %d drops, want exactly one", len(left))
	}
	if left[0].item != ItemVargrPelt || left[0].count != 1 {
		t.Errorf("a dead vargr left %d of item %d, want 1 pelt (%d)",
			left[0].count, left[0].item, ItemVargrPelt)
	}
}

// ---------------------------------------------------------------------------
// What a despawn leaves, which is nothing
// ---------------------------------------------------------------------------

// Loot is the reward for a kill, not for having existed.
//
// Both removals the director performs, because they are two rules in one loop and a
// future refactor could easily route one of them through the death path. Neither may.
func TestADespawnedMobLeavesNothing(t *testing.T) {
	t.Parallel()

	t.Run("taken by the dawn", func(t *testing.T) {
		t.Parallel()

		// No player at all, so nothing is being hunted and the daylight is free to take
		// it — which is the state DawnTakesADraugrThatIsHuntingNobody pins from the
		// director's side.
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, 0.5})
		h.step()

		if _, alive := h.mobState(id); alive {
			t.Fatal("the draugr survived the daylight, so this test never asked its question")
		}
		if got := h.sim.DropCount(); got != 0 {
			t.Errorf("a draugr taken by the dawn left %d drops behind", got)
		}
	})

	t.Run("outside every streamed cube", func(t *testing.T) {
		t.Parallel()

		// A view distance of 1 and a creature well outside it, so the grace runs out
		// while a player is connected — the other half of the rule, and the one that
		// needs the world to be watched by somebody.
		h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
		h.keepNight()
		h.join(1, [3]float32{0.5, 64, 0.5})
		id := h.placeSpeciesAt(vnet.MobKindVargr, [3]float64{500.5, 64, 500.5})

		h.advance(int(h.sim.mobDespawnTicks) + 2)
		if _, alive := h.mobState(id); alive {
			t.Fatal("the vargr was still being simulated after the grace, so nothing was despawned")
		}
		if got := h.sim.DropCount(); got != 0 {
			t.Errorf("a vargr nobody could see left %d drops behind", got)
		}
	})
}

// ---------------------------------------------------------------------------
// The lock discipline
// ---------------------------------------------------------------------------

// A kill resolved inside the tick does not deadlock, and its drop is in the very next
// snapshot.
//
// **The deadlock is the hazard this whole design is arranged around**: the swing is
// judged under Sim.mu and spawnDrop takes Sim.mu itself, so a naive spawn at the point of
// death would wedge the tick goroutine for ever on the first kill anybody made. The
// budget below is a deadlock detector rather than a performance assertion — a wedged tick
// never finishes, and a slow machine finishes in milliseconds.
//
// The "very next" is exact and worth stating: the loot is spawned after the tick that
// killed the creature has already encoded its snapshots, so it appears in the tick after
// it — the same tick a mined block's drop waits.
func TestAKillInsideTheTickNeitherDeadlocksNorMissesTheNextSnapshot(t *testing.T) {
	t.Parallel()

	h, player, out, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})

	done := make(chan struct{})
	go func() {
		defer close(done)
		h.killWithTheStarterBlade(player, id)
	}()
	select {
	case <-done:
	case <-time.After(30 * time.Second):
		t.Fatal("the tick never returned from a kill: spawning loot under Sim.mu deadlocks it")
	}

	if drops := len(out.snapshotDrops(t)); drops != 0 {
		t.Errorf("the snapshot of the killing tick already carried %d drops; loot spawns after it", drops)
	}

	h.step()
	shown := out.snapshotDrops(t)
	if len(shown) != 1 {
		t.Fatalf("the snapshot after the kill carries %d drops, want the one the draugr left", len(shown))
	}
	if shown[0].ItemID != uint16(ItemBone) || shown[0].Count != 1 {
		t.Errorf("the snapshot carries %d of item %d, want 1 bone (%d)",
			shown[0].Count, shown[0].ItemID, ItemBone)
	}
}

// Loot is an ordinary drop: it ages, it merges with what is already there, and it is
// collected by walking over it. There is no special case for it anywhere.
func TestLootIsAnOrdinaryDropInEveryRespect(t *testing.T) {
	t.Parallel()

	// Far enough that walking is still what collects it — the drop lands outside the
	// pickup radius, which is what lets two kills pile up before anybody takes them.
	corpse := [3]float64{0.5, 64, -1.5}
	h, player, _, first := armedAgainst(t, vnet.MobKindDraugr, corpse)
	h.killWithTheStarterBlade(player, first)

	// A second corpse in the same spot, so what is on the ground is one drop rather than
	// two: merging is the drop path's rule and loot goes through it unchanged. The wait
	// is the blade's own cooldown, which Attack refuses a second fight inside.
	h.advance(int(h.sim.attackCooldown) + 1)
	second := h.placeSpeciesAt(vnet.MobKindDraugr, corpse)
	h.killWithTheStarterBlade(player, second)
	h.step()

	left := h.drops()
	if len(left) != 1 {
		t.Fatalf("two draugr killed in one spot left %d drops, want one merged pile", len(left))
	}
	if left[0].count != 2 {
		t.Errorf("the merged pile holds %d bones, want the 1 each of two kills left", left[0].count)
	}

	// And then it is picked up by walking onto it, through the collector that has no idea
	// where the stack came from. The delay has long since elapsed, which is the other half
	// of "an ordinary drop": loot is not collectable on the tick it appears either.
	h.standAt(player, corpse)
	for range dropPickupDelayTicks + 2 {
		h.step()
		if heldCount(player.InventoryState(), ItemBone) == 2 {
			return
		}
	}
	t.Errorf("the bones were never collected: the pack holds %d and %d are on the ground",
		heldCount(player.InventoryState(), ItemBone), h.sim.DropCount())
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

// The same world leaves the same loot, and a different world does not.
//
// **This is the test the roll's design is arranged around**, and it is spawn_test.go's
// determinism test asked of the other generator: a package-level rand, a reading of the
// wall clock, or a generator advanced outside the lock all pass every other test in this
// file and fail this one.
//
// The rolls are taken directly rather than through a scripted fight, because thirty-two
// of them is what makes "a different seed is a different sequence" a real statement about
// a one-in-two draw rather than a coin landing the same way twice.
func TestTheSameWorldLeavesTheSameLoot(t *testing.T) {
	t.Parallel()

	roll := func(seed int64, times int) []uint16 {
		t.Helper()

		sim, err := NewSim(DefaultTickRate, 8, seed, dropTerrain{groundTop: 63}, refusedEdits{},
			testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
		if err != nil {
			t.Fatalf("NewSim: %v", err)
		}

		sim.mu.Lock()
		defer sim.mu.Unlock()

		id, made := sim.spawnMobLocked(vnet.MobKindDraugr, [3]float64{0.5, 64, 0.5})
		if !made {
			t.Fatal("the simulation refused to place a draugr")
		}

		counts := make([]uint16, 0, times)
		for range times {
			left := sim.rollLootLocked(sim.mobs[id])
			if len(left) != 1 || left[0].item != ItemBone {
				t.Fatalf("a draugr rolled %v, want one line of bones", left)
			}
			counts = append(counts, left[0].count)
		}
		return counts
	}

	first, second := roll(20250820, 32), roll(20250820, 32)
	if !equalCounts(first, second) {
		t.Errorf("the same world rolled %v and then %v", first, second)
	}
	if other := roll(20250821, 32); equalCounts(first, other) {
		t.Errorf("two different worlds rolled the same %v, so the seed is being ignored", first)
	}

	// And the range in the table is the range that comes out: one or two, both of them
	// reachable. Without this the sequence above could be a constant and still agree with
	// itself.
	var sawOne, sawTwo bool
	for _, count := range first {
		switch count {
		case 1:
			sawOne = true
		case 2:
			sawTwo = true
		default:
			t.Fatalf("a draugr left %d bones, and its table says one or two", count)
		}
	}
	if !sawOne || !sawTwo {
		t.Errorf("thirty-two rolls of 1..2 produced one=%v two=%v, so the range is not being rolled",
			sawOne, sawTwo)
	}
}

func equalCounts(a, b []uint16) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// ---------------------------------------------------------------------------
// The patch that mends a blade in the field
// ---------------------------------------------------------------------------

// The whole of the user story, end to end: what a hunt leaves is worked into a patch
// wherever the player is standing, and the patch mends the blade that did the hunting.
//
// **Nothing in repair.go was touched to make this work.** The patch is a repair kit
// because its registry row says it restores something, which is what `repairRestore`'s
// own comment promised before the item existed.
func TestWhatAHuntLeavesMendsABladeInTheField(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// Two pelts and nothing built: no forge, no anvil, nowhere to stand.
	h.stockPack(player, ingredient{ItemVargrPelt, 2})
	state, err := h.craft(player, vnet.RecipeIDLeatherPatch)
	if err != nil {
		t.Fatalf("crafting a patch from two pelts in an empty world: %v", err)
	}
	if got := heldCount(state, ItemLeatherPatch); got != 1 {
		t.Fatalf("two pelts made %d patches, want 1", got)
	}
	if got := heldCount(state, ItemVargrPelt); got != 0 {
		t.Errorf("%d pelts survived a craft that costs both of them", got)
	}

	// The patch is in slot 0, where the pelts were. A worn blade goes beside it.
	h.equipWorn(player, 1, ItemRustySword, 40)
	state, err = h.repair(player, 0, 1)
	if err != nil {
		t.Fatalf("mending a worn blade with a patch: %v", err)
	}
	if got := state.Stacks[1].Durability; got != 40+LeatherPatchRestore {
		t.Errorf("the blade came back at %d durability, want %d", got, 40+LeatherPatchRestore)
	}
	if got := heldCount(state, ItemLeatherPatch); got != 0 {
		t.Errorf("%d patches survived the repair that spent one", got)
	}
}

// One pelt is not a patch, and a patch is refused by a blade with nothing to mend — the
// same two refusals the recipe sweep and the sharpening stone already answer, asked of
// the new row so that neither is inherited by assumption.
func TestAPatchIsRefusedWhereAStoneWouldBe(t *testing.T) {
	t.Parallel()

	t.Run("one pelt is not enough", func(t *testing.T) {
		t.Parallel()

		h := newStructureHarness(t)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.stockPack(player, ingredient{ItemVargrPelt, 1})

		before := h.pack(player)
		if _, err := h.craft(player, vnet.RecipeIDLeatherPatch); err == nil {
			t.Error("one pelt made a patch that costs two")
		}
		if after := h.pack(player); after != before {
			t.Error("the refused craft changed the pack")
		}
	})

	t.Run("a full blade has nothing to mend", func(t *testing.T) {
		t.Parallel()

		h := newStructureHarness(t)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.stockPack(player, ingredient{ItemLeatherPatch, 1})
		h.equipWorn(player, 1, ItemRustySword, RustySwordMaxDurability)

		before := h.pack(player)
		if _, err := h.repair(player, 0, 1); err == nil {
			t.Error("a patch was spent on a blade at full durability")
		}
		if after := h.pack(player); after != before {
			t.Error("the refused repair changed the pack")
		}
	})

	t.Run("a resource is not something to mend", func(t *testing.T) {
		t.Parallel()

		// A stack of bones carries `(0, 0)` like every resource, and reading that pair as
		// "worn through" is the mistake `durable()` exists to prevent.
		h := newStructureHarness(t)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.stockPack(player, ingredient{ItemLeatherPatch, 1}, ingredient{ItemBone, 4})

		before := h.pack(player)
		if _, err := h.repair(player, 0, 1); err == nil {
			t.Error("a patch was spent mending a stack of bones")
		}
		if after := h.pack(player); after != before {
			t.Error("the refused repair changed the pack")
		}
	})
}

// The wire shape of a loot drop is the shape every other drop has: a non-zero item and a
// non-zero count, which is what schemas/player.fbs requires of the vector it travels in.
//
// Asked of the encoded states rather than of the simulation's own structs, because the
// contract is about what crosses the wire — and spawnDrop's two refusals are the only
// thing standing between a rolled count of zero and a frame no client may decode.
func TestLootSatisfiesTheDropContract(t *testing.T) {
	t.Parallel()

	h, player, _, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(player, id)
	h.step()

	left := h.drops()
	if len(left) == 0 {
		t.Fatal("nothing was left behind, so this test asked nothing")
	}
	live := make([]*itemDrop, len(left))
	for i := range left {
		live[i] = &left[i]
	}
	for _, state := range dropStates(live) {
		if state.ItemID == uint16(ItemNone) || state.Count == 0 {
			t.Errorf("a loot drop encodes as %d of item %d, and the contract forbids a zero in either",
				state.Count, state.ItemID)
		}
		if _, registered := itemByID(ItemID(state.ItemID)); !registered {
			t.Errorf("a loot drop names item %d, which no registry entry describes", state.ItemID)
		}
	}
}
