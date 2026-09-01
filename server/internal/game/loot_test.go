package game

import (
	"io"
	"log/slog"
	"testing"

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
// killWithTheStarterBlade swings until the named creature is dead, and reports where its
// corpse is.
//
// **There is nothing to step out afterwards, and that is what #441 changed.** The blow
// that empties a creature's health takes it out of Sim.mobs and rolls its container in the
// same call, so the tick this returns on is the tick the corpse exists on. It used to swing
// and then spend MobDeathDuration waiting, because nothing a kill produced existed until
// the reap had run — a test that stopped at the blow was asking its question of a world
// half way through a death and got "nothing on the ground" for an answer whatever the loot
// rules said.
//
// Through the authoritative path — Attack, then the tick — rather than by calling
// damageMobLocked, because the whole question this file asks is what happens when a kill is
// resolved *inside* the tick, under the lock spawnDrop wants.
//
// The position is the corpse's own rather than the last one the body was seen at: they are
// the same value, and reading it back from the corpse is what makes the caller's comparison
// an assertion about the server's answer instead of about the harness's bookkeeping.
func (h *vitalsHarness) killWithTheStarterBlade(p *Player, id uint64) [3]float64 {
	h.t.Helper()

	for blow := 1; blow <= 10; blow++ {
		if _, live := h.mobState(id); !live {
			h.t.Fatalf("creature %d was already gone at blow %d", id, blow)
		}
		// The client tick is taken from the simulation's own rather than from the blow
		// number, because Attack refuses a stale one and a second kill in the same harness
		// would otherwise start counting again from 1.
		if err := h.swing(p, 0, uint32(h.tick)+1); err != nil {
			h.t.Fatalf("swing %d was refused: %v", blow, err)
		}
		h.step()
		if _, live := h.mobState(id); !live {
			return h.corpsePos(id)
		}
		// Past the cooldown before the next one, which is the server's cadence and not
		// the client's.
		h.advance(int(h.sim.attackCooldown))
	}
	h.t.Fatalf("ten blows of the starter blade did not kill creature %d", id)
	return [3]float64{}
}

// corpsePos is where the server put the named corpse, and it fails the test rather than
// answering a zero vector when there is no corpse under that identity.
func (h *vitalsHarness) corpsePos(id uint64) [3]float64 {
	h.t.Helper()

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	c := h.sim.corpses[id]
	if c == nil {
		h.t.Fatalf("no corpse stands under identity %d", id)
	}
	return c.pos
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

// Arrows and the sceptre are the only sinks for bone.
func TestOnlyArrowsAndTheSceptreConsumeBones(t *testing.T) {
	t.Parallel()

	consumers := 0
	for id, r := range recipeTable {
		for _, needed := range r.ingredients {
			if needed.item == ItemBone {
				consumers++
				want := map[vnet.RecipeID]uint16{
					vnet.RecipeIDArrows:        1,
					vnet.RecipeIDWoodenSceptre: 2,
				}
				if needed.count != want[id] {
					t.Errorf("%s costs %d bones, want %d", id, needed.count, want[id])
				}
			}
		}
		if r.product == ItemBone {
			t.Errorf("%s produces bones, which come off a corpse rather than out of a recipe", id)
		}
	}
	if consumers != 2 {
		t.Errorf("bone has %d recipe consumers, want arrows and sceptre", consumers)
	}
	// And a bone is not a weapon, a kit or a structure — the three things a non-zero
	// field in its row would quietly make it.
	bone, registered := itemByID(ItemBone)
	if !registered {
		t.Fatal("the bone is not registered")
	}
	if bone.meleeDamage != 0 || bone.launches != vnet.ProjectileKindUnknown ||
		bone.repairRestore != 0 || bone.maxDurability != 0 {
		t.Errorf("the bone's row is %+v; it is a plain resource", bone)
	}
}

// A corpse is never spent a swing on, and a swing aimed past one reaches what is behind.
//
// **The mechanism changed and the property did not, which is the whole reason this test
// stays.** A killed creature used to lie in Sim.mobs for MobDeathDuration; it was immune —
// damageMobLocked refuses a creature with no health left — but it was still the nearest
// thing in the arc, so every swing was *spent* on the body lying in front of the draugr
// that killed it, and one explicit skip in swingTargetLocked was all that stood between a
// player and that. A corpse is now not in Sim.mobs at all, so the search cannot return one.
// A test that only asserted the skip would have been deleted with it; this one asks the
// question a player asks.
func TestASwingIsNotSpentOnACorpse(t *testing.T) {
	t.Parallel()

	// Two draugr in a line ahead of the player, both inside SwordReach. The near one is
	// killed first, so the far one is what the next swings must reach.
	h, player, _, near := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	far := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, -2.2})

	h.killWithTheStarterBlade(player, near)
	h.sim.mu.Lock()
	_, stillAMob := h.sim.mobs[near]
	_, isACorpse := h.sim.corpses[near]
	h.sim.mu.Unlock()
	if stillAMob || !isACorpse {
		t.Fatal("the near draugr was not left as a corpse, so this test asked nothing")
	}

	h.advance(int(h.sim.attackCooldown))
	if err := h.swing(player, 0, uint32(h.tick)+1); err != nil {
		t.Fatalf("the swing past the body was refused: %v", err)
	}
	h.step()

	behind, live := h.mobState(far)
	if !live {
		t.Fatal("the draugr behind the body left the world")
	}
	if behind.health == draugrRow.maxHealth {
		t.Error("the swing was absorbed by the body in front and never reached the draugr behind it")
	}
}

// A creature killed inside its own telegraph lands nothing, and nothing of it is left
// standing to try again.
//
// **Both halves used to be one half.** The blow that killed it left a body in the world
// that had to be checked for not chasing, not swinging and not still hunting anybody,
// because it was still a mob for two and a half seconds. It is not a mob at all now: the
// assertion is that it is gone from Sim.mobs on the instant, that a corpse stands where it
// was, and that the windup it was one tick from completing never reached the player.
func TestAKilledCreatureLandsNothingAndLeavesNothingStanding(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, -1.0})

	// Committed to a blow that is one tick from landing, and then killed inside that tick.
	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	m.action = vnet.MobActionWindup
	m.actionTicks = 1
	m.target = player.entityID
	stood := m.pos
	h.sim.damageMobLocked(m, draugrRow.maxHealth)
	h.sim.mu.Unlock()

	if _, live := h.mobState(id); live {
		t.Error("a killed draugr is still a mob on the tick of the blow")
	}
	if resting := h.corpsePos(id); resting != stood {
		t.Errorf("the corpse stands at %v; the blow landed at %v", resting, stood)
	}

	h.advance(5)
	if _, live := h.mobState(id); live {
		t.Error("a killed draugr came back into Sim.mobs")
	}
	if moved := h.corpsePos(id); moved != stood {
		t.Errorf("the corpse walked from %v to %v", stood, moved)
	}
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("a killed draugr landed the blow it was winding up: %d health lost", PlayerMaxHealth-got)
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
//
// **Both of a draugr's lines are read, not only the first.** Silver was appended to that
// table after this test was written, and a roll function that kept returning the bone count
// alone would have gone on passing while saying nothing at all about the line beside it —
// including nothing about whether the second draw is seeded, which is the one property this
// test exists for.
func TestTheSameWorldLeavesTheSameLoot(t *testing.T) {
	t.Parallel()

	roll := func(seed int64, times int) [][2]uint16 {
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

		counts := make([][2]uint16, 0, times)
		for range times {
			left := sim.rollLootLocked(sim.mobs[id])
			if len(left.entries) != 1 || left.entries[0].stack.item != ItemBone || left.silver == 0 {
				t.Fatalf("a draugr rolled %+v, want a bone entry and separate silver", left)
			}
			counts = append(counts, [2]uint16{left.entries[0].stack.count, uint16(left.silver)})
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

	// And the ranges in the table are the ranges that come out: one or two bones, both of
	// them reachable, and two to six silver with every value reachable. Without this the
	// sequences above could be constants and still agree with themselves.
	var sawOne, sawTwo bool
	silver := map[uint16]bool{}
	for _, count := range first {
		switch count[0] {
		case 1:
			sawOne = true
		case 2:
			sawTwo = true
		default:
			t.Fatalf("a draugr left %d bones, and its table says one or two", count[0])
		}
		if count[1] < 2 || count[1] > 6 {
			t.Fatalf("a draugr left %d silver, and its table says two to six", count[1])
		}
		silver[count[1]] = true
	}
	if !sawOne || !sawTwo {
		t.Errorf("thirty-two rolls of 1..2 produced one=%v two=%v, so the range is not being rolled",
			sawOne, sawTwo)
	}
	if len(silver) != 5 {
		t.Errorf("thirty-two rolls of 2..6 produced %v, so the whole range is not being rolled",
			silver)
	}
}

func equalCounts(a, b [][2]uint16) bool {
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
// Silver
// ---------------------------------------------------------------------------

// The reserved coin id, its absent row, and the creatures' answer to "is there money on this one".
//
// Pinned by name for the reason every id before it is: iota renumbers everything after an
// insertion, and deleting this hole would reinterpret every persisted id after it.
func TestSilverIsReservedCurrencyDroppedOnlyByDraugr(t *testing.T) {
	t.Parallel()

	if ItemSilver != 35 {
		t.Errorf("silver is item %d, want the appended wire id 35", ItemSilver)
	}
	if _, registered := itemByID(ItemSilver); registered {
		t.Fatal("silver has an inventory registry row")
	}

	// Only the draugr carries any, which is what makes money something the night pays for.
	for kind, def := range mobRegistry {
		var silver int
		for _, roll := range def.loot {
			if !roll.silver {
				continue
			}
			silver++
			if roll.item != ItemNone {
				t.Errorf("%s represents purse silver as item %d", kind, roll.item)
			}
			if roll.min != 2 || roll.max != 6 {
				t.Errorf("%s leaves %d..%d silver, want 2..6", kind, roll.min, roll.max)
			}
		}
		wantLines := 0
		if kind == vnet.MobKindDraugr {
			wantLines = 1
		}
		if silver != wantLines {
			t.Errorf("%s has %d silver loot lines, want %d", kind, silver, wantLines)
		}
	}

	// And nothing crafts it or breaks out of the ground into it: a draugr is the only
	// channel, which is what "no silver from mining, chests or quests" means in code.
	for id, r := range recipeTable {
		if r.product == ItemSilver {
			t.Errorf("%s produces silver, which comes off a corpse rather than out of a recipe", id)
		}
		for _, needed := range r.ingredients {
			if needed.item == ItemSilver {
				t.Errorf("%s costs silver, and nothing is bought with it yet", id)
			}
		}
	}
	for block, dropped := range blockDrops {
		if dropped == ItemSilver {
			t.Errorf("block %d drops silver, and silver comes off a draugr", block)
		}
	}
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
