package game

import (
	"io"
	"log/slog"
	"math"
	"sync"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// ---------------------------------------------------------------------------
// What a record says, and what a join makes of it
// ---------------------------------------------------------------------------

// TestARecordRestoresTheLifeItCaptured is the round trip this whole issue is for:
// what Record captured is what the next Join hands back, to the number.
//
// It goes through Join rather than comparing two Lives, because "the record is right"
// is not the claim — the claim is that a player built from it *is* the player who left.
// So the second life is read back out of the restored player, which is the only reading
// that could catch a Join that quietly ignored a field.
func TestARecordRestoresTheLifeItCaptured(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{12.5, 70, -8.25})

	// A life worth restoring: moved, turned, hurt, and carrying a pack whose three
	// interesting shapes are all present — a worn blade, a partial stack, and an item
	// somewhere other than slot zero.
	h.sim.mu.Lock()
	player.pos = [3]float64{12.5, 70, -8.25}
	player.yaw = 1.25
	player.health = 61
	player.hunger = 37
	player.experience = 1337
	h.sim.mu.Unlock()

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{
		item: ItemIronSword, count: 1, durability: 37, maxDurability: IronSwordMaxDurability,
	}
	player.inventory.slots[5] = inventoryStack{item: ItemStone, count: 23}
	player.inventory.slots[35] = inventoryStack{item: ItemRawIron, count: 1}
	player.inventory.mu.Unlock()

	saved := player.Record()

	// A second simulation, because a reconnect is not a re-entry: the whole point is
	// that nothing of the first player survives in memory.
	next := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	out := &dropSink{}
	restored, err := next.sim.Join(2, testPlayerID(2), testCharacterName, [3]float32{0, 200, 0}, testAppearance(), &saved, out.deliver)
	if err != nil {
		t.Fatalf("Join with a record: %v", err)
	}

	got := restored.Record()
	if got.Pos != saved.Pos {
		t.Errorf("restored position %v, want %v", got.Pos, saved.Pos)
	}
	if got.Yaw != saved.Yaw {
		t.Errorf("restored yaw %v, want %v", got.Yaw, saved.Yaw)
	}
	if got.Health != saved.Health {
		t.Errorf("restored health %d, want %d", got.Health, saved.Health)
	}
	if got.Hunger != saved.Hunger {
		t.Errorf("restored hunger %d, want %d", got.Hunger, saved.Hunger)
	}
	if got.Experience != saved.Experience {
		t.Errorf("restored experience %d, want %d", got.Experience, saved.Experience)
	}
	for slot := range saved.Slots {
		if got.Slots[slot] != saved.Slots[slot] {
			t.Errorf("restored slot %d is %+v, want %+v", slot, got.Slots[slot], saved.Slots[slot])
		}
	}

	// The pack the session sends on join is the same value, through the path that has
	// always sent it: a restored player's first InventoryState is what they were
	// carrying, not a starter sword.
	state := restored.InventoryState()
	if len(state.Stacks) != int(protocol.InventorySlots) {
		t.Fatalf("InventoryState has %d slots, want %d", len(state.Stacks), protocol.InventorySlots)
	}
	for slot, stack := range state.Stacks {
		if stack != saved.Slots[slot] {
			t.Errorf("the wire state for slot %d is %+v, want %+v", slot, stack, saved.Slots[slot])
		}
	}
}

func TestARecordRestoresAWornChestItemAndRejectsItInTheWrongSlot(t *testing.T) {
	const testChest ItemID = 64_990
	itemRegistry[testChest] = itemDefinition{
		maxStack: 1, maxDurability: 25, wornAt: wornChest,
	}
	t.Cleanup(func() { delete(itemRegistry, testChest) })

	life := Life{Pos: [3]float64{0.5, 64, 0.5}, Health: PlayerMaxHealth, Hunger: PlayerMaxHunger}
	life.Slots[equipmentChest] = protocol.InventoryStack{
		ItemID: uint16(testChest), Count: 1, Durability: 17, MaxDurability: 25,
	}
	if err := life.Validate(); err != nil {
		t.Fatalf("the chest item in slot %d was refused: %v", equipmentChest, err)
	}

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	out := &dropSink{}
	player, err := h.sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 64, 0.5}, testAppearance(), &life, out.deliver)
	if err != nil {
		t.Fatalf("Join with worn chest item: %v", err)
	}
	if got := player.Record().Slots[equipmentChest]; got != life.Slots[equipmentChest] {
		t.Errorf("restored chest slot = %+v, want %+v", got, life.Slots[equipmentChest])
	}

	misplaced := life
	misplaced.Slots[equipmentChest] = protocol.InventoryStack{}
	misplaced.Slots[equipmentHead] = life.Slots[equipmentChest]
	if err := misplaced.Validate(); err == nil {
		t.Fatal("Validate accepted a chest item in the head slot")
	}
}

// A restored player settles the same way a new one does, and keeps facing where they
// were facing until their client says otherwise.
//
// onGround false is the part that matters. A stored position was written wherever the
// player stood — which may be on the ground, in the air, or a hair inside a block after
// a rounding — and pretending they are standing would let them accumulate no fall speed
// while the world under them loaded. Falling is the same code path as every other
// landing.
func TestARestoredPlayerSettlesLikeANewOne(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	life := Life{Pos: [3]float64{4.5, 200, 4.5}, Yaw: -2, Health: PlayerMaxHealth}

	out := &dropSink{}
	player, err := h.sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0, 64, 0}, testAppearance(), &life, out.deliver)
	if err != nil {
		t.Fatalf("Join: %v", err)
	}

	h.sim.mu.Lock()
	grounded, yaw, spawn := player.onGround, player.yaw, player.spawn
	h.sim.mu.Unlock()

	if grounded {
		t.Error("a restored player joined already standing")
	}
	if yaw != life.Yaw {
		t.Errorf("restored yaw %v, want %v", yaw, life.Yaw)
	}
	// The join spawn is what Join was given, not what the record held: restoring a
	// position is not moving somebody's respawn point to wherever they logged out.
	if spawn != ([3]float64{0, 64, 0}) {
		t.Errorf("the respawn point moved to %v; it should still be the join spawn", spawn)
	}

	// One tick with no input from the client at all: the yaw is still the restored one
	// rather than snapping to north.
	h.step()
	h.sim.mu.Lock()
	turned := player.yaw
	h.sim.mu.Unlock()
	if turned != life.Yaw {
		t.Errorf("after a tick with no input the yaw is %v, want the restored %v", turned, life.Yaw)
	}
}

// A player with no record joins exactly as they always did.
func TestAPlayerWithNoRecordJoinsWithTheStarterPack(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	life := player.Record()
	if life.Health != PlayerMaxHealth {
		t.Errorf("a new player's health is %d, want %d", life.Health, PlayerMaxHealth)
	}
	if life.Hunger != PlayerMaxHunger {
		t.Errorf("a new player's hunger is %d, want %d", life.Hunger, PlayerMaxHunger)
	}
	if life.Experience != 0 {
		t.Errorf("a new player's experience is %d, want 0", life.Experience)
	}
	if life.Pos != ([3]float64{0.5, 64, 0.5}) {
		t.Errorf("a new player's position is %v, want the join spawn", life.Pos)
	}
	if got, want := life.Slots[0], starterSword(); got != want {
		t.Errorf("slot 0 is %+v, want the starter sword %+v", got, want)
	}
	for slot := 1; slot < int(protocol.InventorySlots); slot++ {
		if life.Slots[slot] != (protocol.InventoryStack{}) {
			t.Errorf("slot %d is %+v, want empty", slot, life.Slots[slot])
		}
	}
}

// ---------------------------------------------------------------------------
// A record always describes a living player
// ---------------------------------------------------------------------------

// TestTheRecordOfADeadPlayerIsTheirRespawn is the acceptance criterion in one test:
// quitting while dead is neither an escape from the death nor a second charge for it.
//
// The player is killed and captured *before* the tick has had a chance to charge the
// penalty, which is the case the teardown path actually faces — a session that ends the
// instant its player dies has no further tick coming.
func TestTheRecordOfADeadPlayerIsTheirRespawn(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// Somewhere other than the spawn, so "at the respawn position" is a claim with
	// something to fail against.
	h.sim.mu.Lock()
	player.pos = [3]float64{40, 80, -40}
	player.hunger = 10
	player.experience = 1337
	h.sim.mu.Unlock()

	h.hurt(player, PlayerMaxHealth)
	h.sim.mu.Lock()
	dead, charged := !player.alive(), player.penaltyApplied
	h.sim.mu.Unlock()
	if !dead {
		t.Fatal("the player did not die")
	}
	if charged {
		t.Fatal("the penalty was charged before any record was taken; this test proves nothing")
	}

	life := player.Record()

	wantHealth := maxHealthFor(levelFor(life.Experience))
	if life.Health != wantHealth {
		t.Errorf("a dead player's record holds %d health, want a full %d", life.Health, wantHealth)
	}
	if life.Hunger != RespawnHungerFloor {
		t.Errorf("a dead player's record holds %d hunger, want the respawn floor %d", life.Hunger, RespawnHungerFloor)
	}
	if life.Experience != 1337 {
		t.Errorf("a dead player's record holds %d experience, want the lifetime total 1337", life.Experience)
	}
	if life.Pos != ([3]float64{0.5, 64, 0.5}) {
		t.Errorf("a dead player's record holds position %v, want the respawn position", life.Pos)
	}
	// The starter blade at four fifths of full, once. The number is the one wornByDeath
	// produces, restated here so a change to the penalty fails this test rather than
	// silently agreeing with itself.
	if got, want := life.Slots[0].Durability, wornByDeath(RustySwordMaxDurability); got != want {
		t.Errorf("the recorded blade has %d durability, want %d", got, want)
	}
	if want := uint16(80); life.Slots[0].Durability != want {
		t.Errorf("the recorded blade has %d durability, want the -20%% of %d", life.Slots[0].Durability, want)
	}

	// Charged once, and charging is what makes it once: a second record — and the tick
	// that follows — must not spend it again.
	second := player.Record()
	if second.Slots[0].Durability != life.Slots[0].Durability {
		t.Errorf("a second record charged the death again: %d, want %d",
			second.Slots[0].Durability, life.Slots[0].Durability)
	}
	h.advance(int(DefaultTickRate) * 5)
	h.sim.mu.Lock()
	afterRespawn := player.inventoryDurabilityLocked(0)
	h.sim.mu.Unlock()
	if afterRespawn != life.Slots[0].Durability {
		t.Errorf("the respawn that followed charged the death again: %d, want %d",
			afterRespawn, life.Slots[0].Durability)
	}
}

// The other order, and the one the autosave usually sees: the tick charged the penalty
// on the way to a respawn, and the record taken afterwards must not charge it twice.
func TestARecordAfterThePenaltyDoesNotChargeItTwice(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.hurt(player, PlayerMaxHealth)
	// One tick is all the penalty needs: advanceVitalsLocked charges it on the first
	// tick after the death, and the countdown keeps the player dead for three seconds.
	h.step()

	h.sim.mu.Lock()
	charged, stillDead := player.penaltyApplied, !player.alive()
	h.sim.mu.Unlock()
	if !charged {
		t.Fatal("the tick did not charge the penalty")
	}
	if !stillDead {
		t.Fatal("the player respawned before the record was taken; this test proves nothing")
	}

	life := player.Record()
	if got, want := life.Slots[0].Durability, wornByDeath(RustySwordMaxDurability); got != want {
		t.Errorf("the recorded blade has %d durability, want %d charged exactly once", got, want)
	}
	if want := maxHealthFor(levelFor(life.Experience)); life.Health != want {
		t.Errorf("a dead player's record holds %d health, want a full %d", life.Health, want)
	}
}

// The tent, because respawnPositionLocked is reused rather than copied — a record
// written while dead has to come back to the same place a respawn would.
//
// It deliberately does not care *how* the tent is found. That lookup is legacy PR 148's to
// change from an entity id to an identity, and this test passes either way because it
// only ever has one player.
func TestADeadPlayersRecordComesBackToTheirTent(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.give(player, 0, ItemTent, 1)

	anchor := [3]int32{2, 63, 0}
	if _, _, err := player.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		t.Fatalf("PlaceStructure: %v", err)
	}

	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()

	life := player.Record()
	want := [3]float64{float64(anchor[0]) + 0.5, float64(anchor[1]) + 1, float64(anchor[2]) + 0.5}
	if life.Pos != want {
		t.Errorf("a dead player's record holds %v, want the tent at %v", life.Pos, want)
	}
}

// ---------------------------------------------------------------------------
// A record is refused whole
// ---------------------------------------------------------------------------

// Every shape of a record this build will not restore, and the rule is the same for all
// of them: refused as a whole, never repaired, never partly believed.
func TestValidateRefusesARecordThisBuildCannotHaveWritten(t *testing.T) {
	t.Parallel()

	sound := func() Life {
		life := Life{Pos: [3]float64{1, 64, 2}, Yaw: 0.5, Health: 42, Hunger: 42}
		life.Slots[0] = protocol.InventoryStack{
			ItemID: uint16(ItemRustySword), Count: 1,
			Durability: 40, MaxDurability: RustySwordMaxDurability,
		}
		life.Slots[1] = protocol.InventoryStack{ItemID: uint16(ItemStone), Count: 12}
		return life
	}

	// The guard the whole table rests on: if this stopped being valid, every case
	// below would pass for the wrong reason.
	if err := sound().Validate(); err != nil {
		t.Fatalf("a sound record was refused: %v", err)
	}

	damage := map[string]func(*Life){
		"a NaN position": func(l *Life) { l.Pos[1] = math.NaN() },
		"an infinite position": func(l *Life) {
			l.Pos[0] = math.Inf(-1)
		},
		"a NaN yaw":       func(l *Life) { l.Yaw = math.NaN() },
		"an infinite yaw": func(l *Life) { l.Yaw = math.Inf(1) },
		// Finite, and still not a position: it narrows to +Inf in the welcome's spawn and
		// does not fit the int64 the chunk feed floors it into.
		"a position past the edge of the world": func(l *Life) { l.Pos[0] = 1e300 },
		"a position just past the edge":         func(l *Life) { l.Pos[2] = -(worldLimit + 1) },
		// The same shape one field along: a heading no wrapAngle ever produced.
		"a yaw that was never wrapped": func(l *Life) { l.Yaw = 1e300 },
		"a yaw just past a half turn":  func(l *Life) { l.Yaw = math.Nextafter(math.Pi, 4) },
		// Zero is not a corpse to restore: a record always describes a living player,
		// so a zero here is a record this server did not write.
		"no health":                     func(l *Life) { l.Health = 0 },
		"more health than a player has": func(l *Life) { l.Health = PlayerMaxHealth + 1 },
		"more hunger than a player has": func(l *Life) { l.Hunger = PlayerMaxHunger + 1 },
		"more experience than the cap":  func(l *Life) { l.Experience = ExperienceCap + 1 },
		"an unknown item id": func(l *Life) {
			l.Slots[2] = protocol.InventoryStack{ItemID: 60000, Count: 1}
		},
		"an item with no count": func(l *Life) {
			l.Slots[2] = protocol.InventoryStack{ItemID: uint16(ItemStone)}
		},
		"a count with no item": func(l *Life) {
			l.Slots[2] = protocol.InventoryStack{Count: 4}
		},
		"durability in an empty slot": func(l *Life) {
			l.Slots[2] = protocol.InventoryStack{Durability: 1, MaxDurability: 2}
		},
		"durability above the maximum": func(l *Life) {
			l.Slots[0].Durability = l.Slots[0].MaxDurability + 1
		},
		"a stack of two swords": func(l *Life) { l.Slots[0].Count = 2 },
	}

	for name, break_ := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			life := sound()
			break_(&life)
			if err := life.Validate(); err == nil {
				t.Fatal("Validate accepted a record this build cannot have written")
			}
		})
	}

	emptyReserve := sound()
	emptyReserve.Hunger = 0
	if err := emptyReserve.Validate(); err != nil {
		t.Errorf("a living record at zero hunger was refused: %v", err)
	}
}

func TestValidateUsesTheMaximumForTheStoredLevel(t *testing.T) {
	t.Parallel()

	levelFive := experienceBefore(5)
	accepted := Life{Health: maxHealthFor(5), Experience: levelFive}
	if err := accepted.Validate(); err != nil {
		t.Fatalf("level-five maximum was refused: %v", err)
	}

	tooHealthy := accepted
	tooHealthy.Health++
	if err := tooHealthy.Validate(); err == nil {
		t.Fatal("health above the level-five maximum was accepted")
	}

	capped := Life{Health: maxHealthFor(MaxLevel), Experience: ExperienceCap}
	if err := capped.Validate(); err != nil {
		t.Fatalf("level-30 maximum was refused: %v", err)
	}
}

// The bounds are not tidiness. What they buy is that every narrowing downstream of
// Validate survives, which is the property the welcome and the chunk feed depend on and
// neither of them re-checks: a position this accepts is still finite as the float32
// ServerWelcome.spawn carries, and still exact as the int64 the chunk feed floors it
// into. The world's own edge is the interesting input, because it is the largest one
// that must pass.
func TestAnAcceptedRecordSurvivesEveryNarrowingBelowIt(t *testing.T) {
	t.Parallel()

	edge := Life{
		Pos:    [3]float64{worldLimit, -worldLimit, worldLimit - 0.5},
		Yaw:    math.Pi,
		Health: PlayerMaxHealth,
	}
	if err := edge.Validate(); err != nil {
		t.Fatalf("a record at the world's edge was refused: %v", err)
	}

	for axis, value := range edge.Pos {
		// float32, as placementSpawn narrows it for the welcome.
		if narrowed := float64(float32(value)); math.IsNaN(narrowed) || math.IsInf(narrowed, 0) {
			t.Errorf("position axis %d is %v, which narrows to %v", axis, value, narrowed)
		}
		// int64, as the chunk feed floors it. Round-tripping catches the overflow, which
		// is implementation-defined rather than an error a caller could notice.
		if voxel := int64(math.Floor(value)); float64(voxel) != math.Floor(value) {
			t.Errorf("position axis %d is %v, which floors to an int64 of %d", axis, value, voxel)
		}
	}
	// And the yaw, as every entity frame narrows it.
	if narrowed := float64(float32(edge.Yaw)); math.IsNaN(narrowed) || math.IsInf(narrowed, 0) {
		t.Errorf("yaw %v narrows to %v", edge.Yaw, narrowed)
	}
}

// The refusal reaches the simulation too, and that is not redundancy for its own sake:
// Join is the boundary a stored life crosses into the physics, and the only thing
// between a file on a disk and a NaN in the integrator is somebody having checked.
func TestJoinRefusesAnInvalidRecord(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	life := Life{Pos: [3]float64{0, math.NaN(), 0}, Health: PlayerMaxHealth}

	if _, err := h.sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0, 64, 0}, testAppearance(), &life, func([]byte) bool { return true }); err == nil {
		t.Fatal("Join admitted a player from a record with a NaN position")
	}
	if h.sim.Count() != 0 {
		t.Error("a refused record left a player in the simulation")
	}
}

// ---------------------------------------------------------------------------
// The autosave's capture
// ---------------------------------------------------------------------------

// Records answers for every connected player, keyed by the identity the record will be
// stored under — not by the entity id, which names one connection and not one player.
func TestRecordsAnswersForEveryConnectedPlayer(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	first, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	second, _ := h.join(2, [3]float32{8.5, 64, 8.5})

	h.sim.mu.Lock()
	second.health = 30
	h.sim.mu.Unlock()

	records := h.sim.Records()
	if len(records) != 2 {
		t.Fatalf("Records answered for %d players, want 2", len(records))
	}
	if got, ok := records[first.PlayerID()]; !ok {
		t.Error("the first player has no record")
	} else if got.Health != PlayerMaxHealth {
		t.Errorf("the first player's record holds %d health, want %d", got.Health, PlayerMaxHealth)
	}
	if got, ok := records[second.PlayerID()]; !ok {
		t.Error("the second player has no record")
	} else if got.Health != 30 {
		t.Errorf("the second player's record holds %d health, want 30", got.Health)
	}

	// A player who has left is not in the answer: their own teardown writes their
	// record, and an autosave that kept writing for them would be writing a life
	// nothing is advancing any more.
	h.sim.Leave(second)
	if len(h.sim.Records()) != 1 {
		t.Error("a player who left is still in Records")
	}
}

// The capture runs beside a live tick, which is the arrangement the autosave actually
// has: one goroutine stepping the simulation and another asking it for records.
//
// It is here for `go test -race` rather than for its assertions. Record takes the
// simulation's lock and then the inventory's, which is the order the tick takes them in
// — the other nesting is the deadlock this pins the absence of.
func TestRecordsRunBesideTheTick(t *testing.T) {
	t.Parallel()

	sim, err := NewSim(DefaultTickRate, 8, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{}, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	out := &dropSink{}
	for id := uint64(1); id <= 3; id++ {
		if _, err := sim.Join(id, testPlayerID(id), testCharacterName, [3]float32{0.5, 64, 0.5}, testAppearance(), nil, out.deliver); err != nil {
			t.Fatalf("Join: %v", err)
		}
	}

	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		for tick := uint64(1); tick <= 200; tick++ {
			sim.Step(tick)
		}
	}()
	go func() {
		defer wg.Done()
		for range 200 {
			if got := len(sim.Records()); got != 3 {
				t.Errorf("Records answered for %d players, want 3", got)
				return
			}
		}
	}()
	wg.Wait()
}

// inventoryDurabilityLocked is one slot's wear, read for a test that already holds the
// simulation's lock. The inventory lock is taken outright because no tick is running.
func (p *Player) inventoryDurabilityLocked(slot int) uint16 {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.inventory.slots[slot].durability
}
