package game

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"math"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// testWorldSeed is the world every simulation in this package's tests is built over.
//
// One value rather than a per-test one, because the seed decides what the spawn
// director draws: a shared constant is what makes "the same world produces the same
// creatures" something the whole package can rely on, and what lets spawn_test.go
// assert exact positions.
const testWorldSeed = 1337

// vitalsHarness drives a simulation at a chosen tick rate, the way the loop does.
//
// The tick rate is a parameter because almost everything here is counted in ticks:
// three seconds of death and two of protection have to be three and two seconds at any
// rate an operator can set, and the safe-fall threshold exists because of what the
// coarsest of them does to the integrator.
type vitalsHarness struct {
	t    *testing.T
	sim  *Sim
	tick uint64
}

func newVitalsHarness(t *testing.T, tickRate uint8, terrain Terrain) *vitalsHarness {
	t.Helper()
	return newVitalsHarnessAt(t, tickRate, terrain, 8)
}

// newVitalsHarnessAt is the same with a chosen view distance, for the visibility tests
// that need the cube to be small enough to stand outside of.
func newVitalsHarnessAt(t *testing.T, tickRate uint8, terrain Terrain, viewDistance uint8) *vitalsHarness {
	t.Helper()
	return newVitalsHarnessOver(t, tickRate, terrain, viewDistance, testWorldSeed)
}

// newVitalsHarnessOver is the same over a chosen world.
//
// The seed is a parameter for exactly one reason, and [testWorldSeed] stays the answer
// everywhere else: a respawn now asks the *world* where the nearest settlement is, and
// "there is no settlement anywhere near here" is a property no single seed can be relied
// on to have at a convenient column. See the respawn tests at the foot of this file.
func newVitalsHarnessOver(t *testing.T, tickRate uint8, terrain Terrain, viewDistance uint8, seed int64) *vitalsHarness {
	t.Helper()

	sim, err := NewSim(tickRate, viewDistance, seed, terrain, refusedEdits{}, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	return &vitalsHarness{t: t, sim: sim}
}

func (h *vitalsHarness) join(entityID uint64, pos [3]float32) (*Player, *dropSink) {
	h.t.Helper()
	return h.joinLife(entityID, pos, nil)
}

func (h *vitalsHarness) joinLife(entityID uint64, pos [3]float32, life *Life) (*Player, *dropSink) {
	h.t.Helper()

	out := &dropSink{}
	player, err := h.sim.Join(entityID, testPlayerID(entityID), testCharacterName, pos, testAppearance(), life, out.deliver)
	if err != nil {
		h.t.Fatalf("Join: %v", err)
	}
	return player, out
}

type testArmourPiece struct {
	item       ItemID
	durability uint16
}

// lifeWearing builds the stored-life shape Join really receives. The registry chooses
// each matching slot and maximum, so these tests cannot put a valid piece on the wrong
// body location or restate its durability ceiling.
func lifeWearing(t *testing.T, pos [3]float32, pieces ...testArmourPiece) Life {
	t.Helper()

	life := Life{
		Pos:    [3]float64{float64(pos[0]), float64(pos[1]), float64(pos[2])},
		Health: PlayerMaxHealth,
		Hunger: PlayerMaxHunger,
		Slots:  [protocol.InventorySlots]protocol.InventoryStack{},
	}
	for _, piece := range pieces {
		definition, registered := itemByID(piece.item)
		if !registered {
			t.Fatalf("test armour item %d is not registered", piece.item)
		}
		var slot int
		switch definition.wornAt {
		case wornHead:
			slot = equipmentHead
		case wornChest:
			slot = equipmentChest
		case wornLegs:
			slot = equipmentLegs
		default:
			t.Fatalf("test armour item %d is not wearable", piece.item)
		}
		life.Slots[slot] = protocol.InventoryStack{
			ItemID:        uint16(piece.item),
			Count:         1,
			Durability:    piece.durability,
			MaxDurability: definition.maxDurability,
		}
	}
	return life
}

func fullTestArmour(item ItemID) testArmourPiece {
	return testArmourPiece{item: item, durability: itemRegistry[item].maxDurability}
}

func (h *vitalsHarness) step() {
	h.tick++
	h.sim.Step(h.tick)
}

func (h *vitalsHarness) advance(n int) {
	for range n {
		h.step()
	}
}

// vitals is one player's authoritative vitals, read under the lock that owns them.
func (h *vitalsHarness) vitals(p *Player) protocol.PlayerVitals {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return p.vitalsLocked()
}

// fallUntilLanded steps until the player is standing, and returns their vitals at that
// moment. Stepping a fixed number of ticks instead is how a test stops discriminating:
// a lethal landing is followed by a countdown and a respawn, and a health reading taken
// afterwards is a full bar either way.
func (h *vitalsHarness) fallUntilLanded(p *Player) protocol.PlayerVitals {
	h.t.Helper()

	for range 600 {
		h.step()
		h.sim.mu.Lock()
		landed := p.onGround
		h.sim.mu.Unlock()
		if landed {
			return h.vitals(p)
		}
	}
	h.t.Fatal("the player never landed")
	return protocol.PlayerVitals{}
}

func (h *vitalsHarness) position(p *Player) [3]float64 {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return p.pos
}

// hurt applies damage the way the simulation does, under the simulation's lock.
func (h *vitalsHarness) hurt(p *Player, amount uint16) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.damageLocked(amount)
}

func TestLevelFiveRegeneratesAndRespawnsToItsOwnMaximum(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	player.experience = experienceBefore(5)
	player.health = PlayerMaxHealth
	player.sinceDamageTicks = h.sim.regenDelayTicks
	for player.health < maxHealthFor(5) {
		player.regenTicks = h.sim.regenIntervalTicks - 1
		player.regenerateLocked()
	}
	player.regenTicks = h.sim.regenIntervalTicks - 1
	player.regenerateLocked()
	regenerated := player.health
	player.damageLocked(regenerated)
	h.sim.mu.Unlock()

	if regenerated != maxHealthFor(5) {
		t.Fatalf("level-five regeneration stopped at %d, want %d", regenerated, maxHealthFor(5))
	}
	h.advance(int(h.sim.deathTicks))
	if got := h.vitals(player); got.Health != maxHealthFor(5) || got.MaxHealth != maxHealthFor(5) {
		t.Errorf("level-five respawn vitals are %d/%d, want %d/%d",
			got.Health, got.MaxHealth, maxHealthFor(5), maxHealthFor(5))
	}
}

func TestTheSameTerminalFallKillsANoviceButNotALevelThirtyPlayer(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	novice, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	veteran, _ := h.join(2, [3]float32{4.5, 64, 0.5})

	h.sim.mu.Lock()
	veteran.experience = ExperienceCap
	veteran.health = veteran.maxHealthLocked()
	damage := fallDamage(TerminalFallSpeed)
	novice.damageLocked(damage)
	veteran.damageLocked(damage)
	noviceAlive := novice.alive()
	veteranAlive, veteranHealth := veteran.alive(), veteran.health
	h.sim.mu.Unlock()

	if noviceAlive {
		t.Error("the level-one player survived the terminal-speed fall")
	}
	if !veteranAlive || veteranHealth != maxHealthFor(MaxLevel)-PlayerMaxHealth {
		t.Errorf("the level-30 player ended alive=%t health=%d, want alive with %d",
			veteranAlive, veteranHealth, maxHealthFor(MaxLevel)-PlayerMaxHealth)
	}
}

// ---------------------------------------------------------------------------
// The formula
// ---------------------------------------------------------------------------

// Every boundary of the one deterministic formula, on the pure function rather than
// through the integrator: what a fall of N blocks arrives at is the integrator's answer
// and varies with the tick rate, while what an impact costs must not vary at all.
func TestFallDamageIsAStepFunctionOfTheImpact(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		impact float64
		want   uint16
	}{
		{impact: 0, want: 0},
		{impact: 10, want: 0},
		// The threshold itself is safe: "at or below" is the contract.
		{impact: SafeFallSpeed, want: 0},
		// And the first speed past it costs the smallest whole step.
		{impact: SafeFallSpeed + 1, want: FallDamagePerSpeed},
		// Floored, not rounded: most of a block per second over is still none of one.
		{impact: SafeFallSpeed + 0.999, want: 0},
		{impact: SafeFallSpeed + 1.999, want: FallDamagePerSpeed},
		{impact: SafeFallSpeed + 10, want: 10 * FallDamagePerSpeed},
		// Terminal velocity is fatal outright, which is what makes a long fall a death
		// rather than a very bad landing.
		{impact: TerminalFallSpeed, want: PlayerMaxHealth},
		// And nothing past it can overflow the health it is subtracted from.
		{impact: 1e9, want: PlayerMaxHealth},
	} {
		if got := fallDamage(tc.impact); got != tc.want {
			t.Errorf("fallDamage(%v) = %d, want %d", tc.impact, got, tc.want)
		}
	}
}

// The acceptance criterion that fixed the threshold, checked against the integrator
// rather than against the arithmetic that chose it.
func TestNoJumpAndNoSpawnSettlementEverHurts(t *testing.T) {
	t.Parallel()

	// Every rate the flag accepts is 1..255; these are its ends and a spread between.
	for _, rate := range []uint8{1, 2, 3, 5, 10, 20, 30, 60, 120, 255} {
		t.Run("tick rate "+string(rune('0'+rate%10)), func(t *testing.T) {
			terrain := dropTerrain{groundTop: 63}
			h := newVitalsHarness(t, rate, terrain)
			// The spawn the world helper produces: SpawnClearance blocks above the
			// surface, which the player then falls through.
			player, _ := h.join(1, [3]float32{0.5, float32(64 + world.SpawnClearance), 0.5})

			// Long enough to settle at every rate, then long enough to jump and land.
			h.advance(int(rate)*2 + 8)
			if got := h.vitals(player).Health; got != PlayerMaxHealth {
				t.Fatalf("the spawn settlement cost %d health at %d Hz", PlayerMaxHealth-got, rate)
			}

			if err := player.Submit(protocol.PlayerInput{ClientTick: 1, Jump: true}); err != nil {
				t.Fatalf("Submit: %v", err)
			}
			h.advance(int(rate)*2 + 8)
			if got := h.vitals(player).Health; got != PlayerMaxHealth {
				t.Errorf("a jump cost %d health at %d Hz", PlayerMaxHealth-got, rate)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Falling
// ---------------------------------------------------------------------------

// A fall long enough to pass the threshold takes health, and the amount is the formula's
// rather than anything this test invents.
func TestALongFallCostsHealth(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 104, 0.5})

	vitals := h.fallUntilLanded(player)
	if vitals.Health == PlayerMaxHealth {
		t.Fatal("a forty-block fall cost nothing")
	}
	if vitals.LifeState != vnet.LifeStateAlive {
		t.Fatalf("a forty-block fall was fatal: %+v", vitals)
	}
	if got := h.position(player)[1]; got < 63 {
		t.Errorf("the player ended at y=%v, below the ground they landed on", got)
	}
}

func TestFullIronDoesNotSoftenAFall(t *testing.T) {
	t.Parallel()

	landedHealth := func(t *testing.T, iron bool) uint16 {
		t.Helper()
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		pos := [3]float32{0.5, 104, 0.5}
		var player *Player
		if iron {
			life := lifeWearing(t, pos,
				fullTestArmour(ItemIronHelm),
				fullTestArmour(ItemIronCuirass),
				fullTestArmour(ItemIronGreaves),
			)
			player, _ = h.joinLife(1, pos, &life)
		} else {
			player, _ = h.join(1, pos)
		}
		return h.fallUntilLanded(player).Health
	}

	naked := landedHealth(t, false)
	armoured := landedHealth(t, true)
	if naked == PlayerMaxHealth {
		t.Fatal("the comparison fall cost nothing, so it cannot discriminate armour")
	}
	if armoured != naked {
		t.Errorf("full iron left %d health after the fall, want the unarmoured %d", armoured, naked)
	}
}

// The maximum-speed fall, which the formula makes fatal by construction.
func TestAFallAtTerminalSpeedKills(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 400, 0.5})

	// Until the landing, not for a fixed span: three hundred blocks at terminal speed is
	// about six seconds, and a run long enough to be safe would also outlast the
	// countdown and find the player alive again on the other side of a respawn.
	var vitals protocol.PlayerVitals
	for range 300 {
		h.step()
		if vitals = h.vitals(player); vitals.LifeState == vnet.LifeStateDead {
			break
		}
	}
	if vitals.LifeState != vnet.LifeStateDead {
		t.Fatalf("a fall from 400 blocks left the player %+v", vitals)
	}
	if vitals.Health != 0 {
		t.Errorf("a dead player has %d health, want 0", vitals.Health)
	}
}

// The regression the collision's conservative reading would otherwise create: an absent
// chunk is solid so a player does not fall out of a world that is still loading, and
// that fiction must not also be a floor to break on.
func TestLandingOnANonResidentChunkNeverHurts(t *testing.T) {
	t.Parallel()

	// Everything at or below the ground is present in the collision's eyes and absent to
	// the resident read — exactly the state of a chunk that has not arrived.
	terrain := dropTerrain{
		groundTop: 63,
		absent:    func(_, y, _ int64) bool { return y <= 63 },
	}
	h := newVitalsHarness(t, DefaultTickRate, terrain)
	player, _ := h.join(1, [3]float32{0.5, 400, 0.5})

	vitals := h.fallUntilLanded(player)
	if vitals.Health != PlayerMaxHealth {
		t.Errorf("waiting for terrain cost %d health", PlayerMaxHealth-vitals.Health)
	}
	if vitals.LifeState != vnet.LifeStateAlive {
		t.Errorf("waiting for terrain was fatal: %+v", vitals)
	}

	// And the velocity was still cancelled, so the player is not holding the fall speed
	// they would arrive with once the chunk lands.
	h.sim.mu.Lock()
	vertical := player.vel[1]
	h.sim.mu.Unlock()
	if vertical != 0 {
		t.Errorf("the player kept %v blocks/s of fall speed against unloaded terrain", vertical)
	}
}

// ---------------------------------------------------------------------------
// Damage, death and the countdown
// ---------------------------------------------------------------------------

func TestDamageClampsAndRefusesTheCasesThatAreNotHits(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// A hit for nothing is not a hit.
	h.hurt(player, 0)
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("zero damage took %d health", PlayerMaxHealth-got)
	}

	h.hurt(player, 30)
	if got := h.vitals(player).Health; got != 70 {
		t.Errorf("health is %d after 30 damage, want 70", got)
	}

	// Clamped at zero rather than wrapped, which is the whole reason the subtraction
	// lives in one place: 200 from 70 in unsigned arithmetic is a very healthy player.
	h.hurt(player, 200)
	vitals := h.vitals(player)
	if vitals.Health != 0 {
		t.Errorf("health is %d after lethal damage, want 0", vitals.Health)
	}
	if vitals.LifeState != vnet.LifeStateDead {
		t.Errorf("life state is %s, want Dead", vitals.LifeState)
	}

	// And the dead do not take damage again.
	h.hurt(player, 10)
	if got := h.vitals(player).RespawnTicks; got != h.sim.deathTicks {
		t.Errorf("damaging a corpse restarted the countdown: %d, want %d", got, h.sim.deathTicks)
	}
}

func poisonWornSummary(p *Player) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	p.worn.armour = ArmourScale - 1
	p.worn.threat = ArmourScale - 1
}

func assertWornSummaryFresh(t *testing.T, p *Player) {
	t.Helper()

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()

	var armour, threat uint16
	for _, stack := range p.inventory.slots[equipmentFirst:] {
		if stack.durability == 0 {
			continue
		}
		definition, registered := itemByID(stack.item)
		if !registered {
			continue
		}
		armour += definition.armour
		threat += definition.threat
	}
	if p.worn.armour != armour || p.worn.threat != threat {
		t.Errorf("cached worn summary is armour=%d threat=%d, fresh slots say armour=%d threat=%d",
			p.worn.armour, p.worn.threat, armour, threat)
	}
}

// This list is the inventory-mutation boundary the cache depends on. Each case drives
// the public entry point (or the tick's death transition), poisons the cache first where
// possible, and compares it with a fresh slot recomputation afterwards. Adding a new
// mutation entry point means making an explicit decision in this list.
func TestEveryInventoryMutationRefreshesTheWornSummary(t *testing.T) {
	pos := [3]float32{0.5, 64, 0.5}

	for _, tc := range []struct {
		name string
		run  func(*testing.T) *Player
	}{
		{
			name: "Join",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := lifeWearing(t, pos, fullTestArmour(ItemIronHelm))
				player, _ := h.joinLife(1, pos, &life)
				return player
			},
		},
		{
			name: "MoveInventory",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := Life{Pos: [3]float64{0.5, 64, 0.5}, Health: PlayerMaxHealth, Hunger: PlayerMaxHunger}
				life.Slots[1] = protocol.InventoryStack{ItemID: uint16(ItemLeatherCap), Count: 1,
					Durability: LeatherArmourMaxDurability, MaxDurability: LeatherArmourMaxDurability}
				player, _ := h.joinLife(1, pos, &life)
				poisonWornSummary(player)
				if _, err := player.MoveInventory(protocol.InventoryMoveRequest{From: 1, To: uint8(equipmentHead), Count: 1}); err != nil {
					t.Fatalf("MoveInventory: %v", err)
				}
				return player
			},
		},
		{
			name: "Repair",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := lifeWearing(t, pos, testArmourPiece{item: ItemLeatherCap, durability: 0})
				life.Slots[1] = protocol.InventoryStack{ItemID: uint16(ItemLeatherPatch), Count: 1}
				player, _ := h.joinLife(1, pos, &life)
				poisonWornSummary(player)
				if _, err := player.Repair(protocol.RepairRequest{KitSlot: 1, TargetSlot: uint8(equipmentHead)}); err != nil {
					t.Fatalf("Repair: %v", err)
				}
				return player
			},
		},
		{
			name: "Craft",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := lifeWearing(t, pos, fullTestArmour(ItemLeatherCap))
				life.Slots[1] = protocol.InventoryStack{ItemID: uint16(ItemVargrPelt), Count: 3}
				player, _ := h.joinLife(1, pos, &life)
				poisonWornSummary(player)
				if _, err := player.Craft(protocol.CraftRequest{Recipe: vnet.RecipeIDLeatherCap}); err != nil {
					t.Fatalf("Craft: %v", err)
				}
				return player
			},
		},
		{
			name: "Consume",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := lifeWearing(t, pos, fullTestArmour(ItemLeatherCap))
				life.Hunger = 0
				life.Slots[1] = protocol.InventoryStack{ItemID: uint16(ItemRawMeat), Count: 1}
				player, _ := h.joinLife(1, pos, &life)
				poisonWornSummary(player)
				if _, err := player.Consume(protocol.ConsumeRequest{Slot: 1}); err != nil {
					t.Fatalf("Consume: %v", err)
				}
				return player
			},
		},
		{
			name: "DropItem",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := lifeWearing(t, pos, fullTestArmour(ItemLeatherCap))
				player, _ := h.joinLife(1, pos, &life)
				poisonWornSummary(player)
				if _, err := player.DropItem(protocol.DropItemRequest{Slot: uint8(equipmentHead)}); err != nil {
					t.Fatalf("DropItem: %v", err)
				}
				return player
			},
		},
		{
			name: "death penalty",
			run: func(t *testing.T) *Player {
				h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
				life := lifeWearing(t, pos, testArmourPiece{item: ItemLeatherCap, durability: 1})
				player, _ := h.joinLife(1, pos, &life)
				poisonWornSummary(player)
				h.hurt(player, PlayerMaxHealth)
				h.step()
				return player
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			assertWornSummaryFresh(t, tc.run(t))
		})
	}
}

func TestDeathHappensOnceAndStopsEverythingInFlight(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, MoveZ: 1, Yaw: 0.5}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()

	h.sim.mu.Lock()
	player.mining = &miningState{pos: [3]int32{1, 63, 0}, block: world.Stone, cost: 10}
	player.mineReset = &miningReset{}
	player.mineCompleting = true
	player.damageLocked(PlayerMaxHealth)
	// The transition is the event, and running it twice would restart the countdown and
	// un-pay the durability penalty. Called directly rather than through a second lethal
	// blow: damageLocked refuses to hurt the dead, so a second blow never reaches here
	// and would leave dieLocked's own guard untested.
	player.respawnTicks--
	player.dieLocked()
	player.damageLocked(PlayerMaxHealth)
	state := struct {
		respawnTicks   uint32
		mining         *miningState
		reset          *miningReset
		completing     bool
		velocity       [3]float64
		intent         intent
		minersRecorded int
	}{
		player.respawnTicks, player.mining, player.mineReset, player.mineCompleting,
		player.vel, player.current, len(h.sim.minersByPos),
	}
	h.sim.mu.Unlock()

	if state.respawnTicks != h.sim.deathTicks-1 {
		t.Errorf("the countdown is %d, want the %d a second death did not reset",
			state.respawnTicks, h.sim.deathTicks-1)
	}
	if state.mining != nil || state.reset != nil || state.completing {
		t.Errorf("death left mining in flight: state=%v reset=%v completing=%v",
			state.mining, state.reset, state.completing)
	}
	if state.minersRecorded != 0 {
		t.Errorf("death left %d entries in the reverse mining index", state.minersRecorded)
	}
	if state.velocity != ([3]float64{}) {
		t.Errorf("death left the velocity at %v", state.velocity)
	}
	if state.intent.moveX != 0 || state.intent.moveZ != 0 || state.intent.jump {
		t.Errorf("death left movement intent %+v", state.intent)
	}
	if state.intent.yaw != 0.5 {
		t.Errorf("death changed the facing to %v; a corpse faces where it fell", state.intent.yaw)
	}
}

// A leaving body is still part of the world, so damage and the ordinary death path
// continue. What stays irrevocable is agency: a respawn inside the linger keeps the
// leaving bit, while Sim.Leave removing a corpse before its countdown ends is the end
// of the only tick path that could ever respawn it.
func TestDeathDuringLeaveLingerCannotRestoreAgencyOrResurrectARemovedPlayer(t *testing.T) {
	t.Parallel()

	t.Run("respawn inside the linger stays inert", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		player.BeginLeaving()
		h.hurt(player, PlayerMaxHealth)

		h.advance(int(h.sim.deathTicks))
		if got := h.vitals(player); got.LifeState != vnet.LifeStateAlive || got.Health != PlayerMaxHealth {
			t.Fatalf("leaving player after the death countdown is %+v, want an ordinary full-health respawn", got)
		}
		if err := player.Submit(protocol.PlayerInput{ClientTick: 1, MoveZ: 1}); err == nil {
			t.Fatal("respawning during leave restored player agency")
		}
		if got := h.sim.Count(); got != 1 {
			t.Fatalf("respawn duplicated the leaving player: simulation holds %d, want 1", got)
		}
	})

	t.Run("removal ends an unfinished countdown", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		player.BeginLeaving()
		h.hurt(player, PlayerMaxHealth)

		h.advance(int(h.sim.deathTicks) - 1)
		before := h.vitals(player)
		if before.LifeState != vnet.LifeStateDead || before.RespawnTicks != 1 {
			t.Fatalf("player just before removal is %+v, want a corpse with one tick left", before)
		}

		h.sim.Leave(player)
		h.advance(int(h.sim.deathTicks) + 1)
		if got := h.sim.Count(); got != 0 {
			t.Fatalf("removed player reappeared after its countdown: simulation holds %d", got)
		}
		if after := h.vitals(player); after != before {
			t.Errorf("removed player's detached state advanced from %+v to %+v", before, after)
		}
	})
}

// The three seconds are three seconds, whatever Step is being called at.
func TestTheDeathCountdownIsThreeSecondsAtEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{1, 5, 20, 60} {
		t.Run("tick rate "+string(rune('0'+rate%10)), func(t *testing.T) {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.hurt(player, PlayerMaxHealth)

			want := deathDurationTicks(rate)
			if got := h.vitals(player).RespawnTicks; got != want {
				t.Fatalf("the countdown starts at %d ticks, want %d", got, want)
			}

			// One tick short of the whole countdown: still dead, and the client still has
			// a number to draw.
			h.advance(int(want) - 1)
			vitals := h.vitals(player)
			if vitals.LifeState != vnet.LifeStateDead {
				t.Fatalf("the player respawned %d ticks early", 1)
			}
			if vitals.RespawnTicks != 1 {
				t.Errorf("the countdown reads %d with one tick left", vitals.RespawnTicks)
			}

			h.step()
			if got := h.vitals(player).LifeState; got != vnet.LifeStateAlive {
				t.Errorf("the player is %s after the full countdown, want Alive", got)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Respawn
// ---------------------------------------------------------------------------

func TestRespawnRestoresTheJoinSpawnAndGrantsProtection(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	spawn := [3]float32{0.5, 66, 0.5}
	player, _ := h.join(1, spawn)

	// Walk away first, so the respawn is a teleport rather than a coincidence.
	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, MoveZ: 1, Yaw: 0}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.advance(40)
	if got := h.position(player); got[2] == float64(spawn[2]) {
		t.Fatal("the player never left their spawn, so the teleport proves nothing")
	}

	h.hurt(player, PlayerMaxHealth)
	h.advance(int(h.sim.deathTicks))

	vitals := h.vitals(player)
	if vitals.LifeState != vnet.LifeStateAlive {
		t.Fatalf("the player is %s after the countdown", vitals.LifeState)
	}
	if vitals.Health != PlayerMaxHealth {
		t.Errorf("the player respawned with %d health, want %d", vitals.Health, PlayerMaxHealth)
	}
	if vitals.RespawnTicks != 0 {
		t.Errorf("a living player carries a countdown of %d", vitals.RespawnTicks)
	}
	if !vitals.Invulnerable {
		t.Error("the respawn granted no protection")
	}

	// The spawn is the one Join was given, in every axis. The vertical settles by
	// falling, which is the same landing every other fall is — so it is the horizontal
	// pair that says the teleport happened.
	pos := h.position(player)
	if pos[0] != float64(spawn[0]) || pos[2] != float64(spawn[2]) {
		t.Errorf("the player respawned at %v, want the join spawn %v", pos, spawn)
	}
}

// Protection is spent by ticks and by nothing else — no wall clock, and no message from
// the client.
func TestRespawnProtectionExpiresFromTicks(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.hurt(player, PlayerMaxHealth)
	h.advance(int(h.sim.deathTicks))

	if !h.vitals(player).Invulnerable {
		t.Fatal("the respawn granted no protection")
	}

	// Damage during protection is refused outright.
	h.hurt(player, 50)
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("a protected player lost %d health", PlayerMaxHealth-got)
	}

	h.advance(int(h.sim.protectionTicks) - 1)
	if !h.vitals(player).Invulnerable {
		t.Error("protection ended one tick early")
	}
	h.step()
	if h.vitals(player).Invulnerable {
		t.Error("protection outlasted its tick count")
	}

	h.hurt(player, 50)
	if got := h.vitals(player).Health; got != 50 {
		t.Errorf("health is %d once protection ended, want 50", got)
	}
}

// A respawn is a teleport, and the streaming goroutine has to hear about it on the tick
// it happens or the player stands in terrain nobody has sent them.
func TestRespawnWakesChunkStreaming(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	spawn := [3]float32{0.5, 66, 0.5}
	player, _ := h.join(1, spawn)

	// Drain the spawn chunk Join published, so what arrives later is the respawn's.
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if _, err := player.NextChunk(ctx); err != nil {
		t.Fatalf("NextChunk: %v", err)
	}

	// Far enough to be a different chunk.
	h.sim.mu.Lock()
	player.pos = [3]float64{200.5, 64, 200.5}
	player.chunk = chunkAt(player.pos)
	h.sim.mu.Unlock()
	h.step()

	h.hurt(player, PlayerMaxHealth)
	h.advance(int(h.sim.deathTicks))

	coord, err := player.NextChunk(ctx)
	if err != nil {
		t.Fatalf("NextChunk after the respawn: %v", err)
	}
	if want := chunkAt([3]float64{float64(spawn[0]), float64(spawn[1]), float64(spawn[2])}); coord != want {
		t.Errorf("the respawn published chunk %+v, want the spawn's %+v", coord, want)
	}
}

// ---------------------------------------------------------------------------
// What the dead may not do
// ---------------------------------------------------------------------------

func TestTheDeadAreRefusedEveryIntent(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.hurt(player, PlayerMaxHealth)

	if err := player.Submit(protocol.PlayerInput{ClientTick: 9, MoveZ: 1}); err == nil {
		t.Error("a dead player's movement was accepted")
	}
	if err := player.Mine(protocol.MineRequest{Pos: [3]int32{1, 63, 0}, HasPos: true, Active: true, ClientTick: 9}, true); err == nil {
		t.Error("a dead player's mining was accepted")
	}
	if _, err := player.Edit(context.Background(), protocol.BlockEditRequest{
		Pos: [3]int32{1, 63, 0}, HasPos: true, Action: vnet.EditActionPlace, Slot: 0, ClientTick: 9,
	}); err == nil {
		t.Error("a dead player's placement was accepted")
	}

	// And a corpse does not drift, however many ticks pass.
	before := h.position(player)
	h.advance(10)
	if after := h.position(player); after != before {
		t.Errorf("a dead player moved from %v to %v", before, after)
	}
}

// A new life resets both ordering guards, not just movement's.
//
// They are separate counters on purpose — movement and mining arrive on separate
// messages with separate idle windows — and resetting one without the other is an
// asymmetry rather than a safety: a client that restarts its counter on a new life would
// have its walking accepted and its mining refused as stale until the count caught up.
func TestANewLifeAcceptsAClientThatRestartedItsTickCounter(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// A long-running session, then a death.
	if err := player.Submit(protocol.PlayerInput{ClientTick: 9000, Yaw: 0}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	if err := player.Mine(protocol.MineRequest{
		Pos: [3]int32{1, 63, 0}, HasPos: true, Active: true, ClientTick: 9000,
	}, true); err != nil {
		t.Fatalf("Mine: %v", err)
	}

	h.hurt(player, PlayerMaxHealth)
	h.advance(int(h.sim.deathTicks))
	if got := h.vitals(player).LifeState; got != vnet.LifeStateAlive {
		t.Fatalf("the player is %s after the countdown", got)
	}

	// A counter that started over. Both intents have to be accepted, or the two guards
	// disagree about what a new life means.
	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, Yaw: 0}); err != nil {
		t.Errorf("movement from a restarted counter was refused: %v", err)
	}
	if err := player.Mine(protocol.MineRequest{
		Pos: [3]int32{1, 63, 0}, HasPos: true, Active: true, ClientTick: 1,
	}, true); err != nil {
		t.Errorf("mining from a restarted counter was refused: %v", err)
	}
}

// A placement that was legal when it was asked for, by a player who died while its chunk
// was being generated.
//
// The check before generation cannot see this: it runs before the wait. What refuses it
// is the second one, in the guard the editor calls after the terrain is ready and before
// the write — and the staged editor is what makes the window a test can stand inside
// rather than a race to reproduce.
func TestAPlacementIsRefusedWhenThePlayerDiesWhileItsChunkLoads(t *testing.T) {
	t.Parallel()

	editor := &stagedEditor{
		generationStarted: make(chan struct{}),
		finishGeneration:  make(chan struct{}),
		guardAcquired:     make(chan struct{}),
		finishWrite:       make(chan struct{}),
		current:           world.Air,
	}
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, emptyTerrain{}, editor, testEntityIDs(),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
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

	// The tick kills them while the chunk is still loading.
	sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	sim.mu.Unlock()

	close(editor.finishGeneration)

	select {
	case editErr := <-result:
		if editErr == nil {
			t.Fatal("a player who died mid-generation still placed their block")
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for the edit to be refused")
	}

	// And the slot was not spent on it.
	if got := player.InventoryState().Stacks[0].Count; got != 1 {
		t.Errorf("the refused placement left %d in slot 0, want the 1 it started with", got)
	}
}

// ---------------------------------------------------------------------------
// The durability penalty
// ---------------------------------------------------------------------------

func TestDeathSpendsDurabilityOnceAndKeepsEveryItem(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	player.inventory.slots[1] = stackOf(ItemStone, 40)
	player.inventory.mu.Unlock()

	h.hurt(player, PlayerMaxHealth)
	h.step()

	state := player.InventoryState()
	wantSword := protocol.InventoryStack{
		ItemID:        uint16(ItemRustySword),
		Count:         1,
		Durability:    wornByDeath(RustySwordMaxDurability),
		MaxDurability: RustySwordMaxDurability,
	}
	if got := state.Stacks[0]; got != wantSword {
		t.Errorf("slot 0 is %+v after one death, want %+v", got, wantSword)
	}
	wantStone := protocol.InventoryStack{ItemID: uint16(ItemStone), Count: 40}
	if got := state.Stacks[1]; got != wantStone {
		t.Errorf("death changed slot 1 to %+v, want the resources untouched %+v", got, wantStone)
	}

	// Every remaining tick of the countdown must not spend it again.
	h.advance(int(h.sim.deathTicks))
	if got := player.InventoryState().Stacks[0]; got != wantSword {
		t.Errorf("slot 0 is %+v after the whole countdown, want the one penalty %+v", got, wantSword)
	}

	// A second death is a second penalty, though — the operation is per death, not once
	// per session. Past the respawn protection first: damage during it is refused, and a
	// blow that lands on nobody would leave this asserting the first penalty twice.
	h.advance(int(h.sim.protectionTicks))
	if h.vitals(player).Invulnerable {
		t.Fatal("respawn protection outlasted its tick count, so the second blow lands on nothing")
	}
	h.hurt(player, PlayerMaxHealth)
	h.step()
	twice := wornByDeath(wornByDeath(RustySwordMaxDurability))
	if got := player.InventoryState().Stacks[0].Durability; got != twice {
		t.Errorf("durability is %d after two deaths, want %d", got, twice)
	}
}

// The tick never waits for a session goroutine, and it never comes back from a death
// without having paid for it.
func TestAContendedInventoryDefersTheDeathPenaltyAndTheRespawn(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.hurt(player, PlayerMaxHealth)

	// A session goroutine holding the inventory across the whole countdown.
	player.inventory.mu.Lock()
	h.advance(int(h.sim.deathTicks) + 5)

	vitals := h.vitals(player)
	if vitals.LifeState != vnet.LifeStateDead {
		t.Errorf("the player respawned without paying the penalty: %+v", vitals)
	}
	if vitals.RespawnTicks != 0 {
		t.Errorf("the countdown reads %d; it is a clock and should have run out", vitals.RespawnTicks)
	}
	player.inventory.mu.Unlock()

	// The very next tick pays it and respawns.
	h.step()
	if got := h.vitals(player).LifeState; got != vnet.LifeStateAlive {
		t.Errorf("the player is %s once the inventory was free, want Alive", got)
	}
	want := wornByDeath(RustySwordMaxDurability)
	if got := player.InventoryState().Stacks[0].Durability; got != want {
		t.Errorf("durability is %d, want the single penalty %d", got, want)
	}
}

// Ticking while a session moves items is the shape that goes wrong under -race: the
// tick takes the inventory lock without waiting, and a death lands somewhere in it.
func TestDeathUnderConcurrentInventoryMovement(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	var wg sync.WaitGroup
	stop := make(chan struct{})
	wg.Add(1)
	go func() {
		defer wg.Done()
		for {
			select {
			case <-stop:
				return
			default:
			}
			// Slot 0 holds the blade; moving it about is what a player rearranging their
			// hotbar does while they are being killed.
			_, _ = player.MoveInventory(protocol.InventoryMoveRequest{From: 0, To: 5, Count: 1})
			_, _ = player.MoveInventory(protocol.InventoryMoveRequest{From: 5, To: 0, Count: 1})
		}
	}()

	// Three deaths, each waited out rather than counted in ticks. Both halves of that
	// are what the contention above breaks: the tick asks for the inventory with
	// TryLock and never waits, so a penalty — and with it the respawn — can be deferred
	// for as long as the goroutine keeps the lock busy. A fixed advance therefore ends
	// somewhere unpredictable, and the next blow lands during respawn protection and is
	// refused, leaving this counting two penalties for three deaths. It failed about one
	// run in eight before it was written this way.
	for death := range 3 {
		for h.vitals(player).Invulnerable {
			h.step()
		}
		h.hurt(player, PlayerMaxHealth)
		if got := h.vitals(player).LifeState; got != vnet.LifeStateDead {
			t.Fatalf("blow %d left the player %s", death+1, got)
		}

		alive := false
		for range 5000 {
			h.step()
			if h.vitals(player).LifeState == vnet.LifeStateAlive {
				alive = true
				break
			}
		}
		if !alive {
			t.Fatalf("death %d never ended; the penalty was deferred for 5000 ticks", death+1)
		}
	}

	close(stop)
	wg.Wait()

	if got := h.vitals(player).LifeState; got != vnet.LifeStateAlive {
		t.Errorf("the player is %s after three deaths and their countdowns", got)
	}
	// Three deaths, three penalties, wherever the blade ended up.
	want := wornByDeath(wornByDeath(wornByDeath(RustySwordMaxDurability)))
	state := player.InventoryState()
	found := false
	for _, stack := range state.Stacks {
		if stack.ItemID != uint16(ItemRustySword) {
			continue
		}
		found = true
		if stack.Durability != want {
			t.Errorf("the blade is at %d durability, want %d for three deaths", stack.Durability, want)
		}
	}
	if !found {
		t.Error("the blade stopped existing")
	}
}

// ---------------------------------------------------------------------------
// Health that comes back on its own
// ---------------------------------------------------------------------------

// stepFor advances the harness by a duration, in the ticks the rate makes of it.
func (h *vitalsHarness) stepFor(d time.Duration) {
	h.t.Helper()

	for range ticksFor(d, DefaultTickRate) {
		h.step()
	}
}

// The delay is quiet since the last hit, and the first point lands one interval after it.
//
// **The off-by-one is asserted deliberately rather than discovered.** Regeneration resumes
// at HealthRegenDelay and produces its first point HealthRegenInterval later, so a test
// that stepped exactly the delay and expected a point would be describing a different
// design — and one that stepped "about six seconds" would pass whichever design was built.
func TestHealthComesBackOneIntervalAfterTheDelay(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.hurt(player, 40)
	if got := h.vitals(player).Health; got != PlayerMaxHealth-40 {
		t.Fatalf("health after the hit is %d, want %d", got, PlayerMaxHealth-40)
	}

	// One tick short of the delay plus an interval: still nothing given back.
	h.stepFor(HealthRegenDelay + HealthRegenInterval)
	h.step() // the tick the point lands on is the one after that span
	if got := h.vitals(player).Health; got != PlayerMaxHealth-39 {
		t.Fatalf("health after the delay and one interval is %d, want %d", got, PlayerMaxHealth-39)
	}

	// And then one a second.
	h.stepFor(3 * HealthRegenInterval)
	if got := h.vitals(player).Health; got != PlayerMaxHealth-36 {
		t.Errorf("health three intervals later is %d, want %d", got, PlayerMaxHealth-36)
	}
}

// Nothing comes back inside the delay, and a second hit starts it over.
//
// This is the property the five seconds exist for: a draugr swings about every 1.5s, so a
// player being hit never reaches the threshold at all. Asserted by hitting twice with less
// than the delay between, which is what a fight is.
func TestRegenerationNeverTicksInsideAFight(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.hurt(player, 30)
	h.stepFor(HealthRegenDelay - HealthRegenInterval)
	if got := h.vitals(player).Health; got != PlayerMaxHealth-30 {
		t.Fatalf("health moved inside the delay: %d, want %d", got, PlayerMaxHealth-30)
	}

	// A second hit before the threshold, the way a fight lands them.
	h.hurt(player, 10)
	h.stepFor(HealthRegenDelay - HealthRegenInterval)
	if got := h.vitals(player).Health; got != PlayerMaxHealth-40 {
		t.Errorf("the second hit did not restart the delay: health %d, want %d", got, PlayerMaxHealth-40)
	}
}

// It stops at the maximum, and a dead player gains nothing.
//
// The second half is a property of the call site rather than of a check: regenerateLocked
// runs on advanceVitalsLocked's alive branch and nowhere else, so a corpse that healed
// would mean the call had moved.
func TestRegenerationIsBoundedAndNeverResurrects(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	full, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stepFor(HealthRegenDelay + 5*HealthRegenInterval)
	if got := h.vitals(full).Health; got != PlayerMaxHealth {
		t.Errorf("an unhurt player's health is %d, want the maximum %d", got, PlayerMaxHealth)
	}

	dead, _ := h.join(2, [3]float32{0.5, 64, 0.5})
	h.hurt(dead, PlayerMaxHealth)
	if got := h.vitals(dead); got.Health != 0 || got.LifeState != vnet.LifeStateDead {
		t.Fatalf("the second player is %+v, want dead at zero", got)
	}
	// Long enough to have healed several points had anything been running, and short of
	// the respawn that would legitimately restore them.
	for range ticksFor(HealthRegenDelay+3*HealthRegenInterval, DefaultTickRate) {
		h.sim.mu.Lock()
		dead.respawnTicks = h.sim.deathTicks // hold the countdown open
		h.sim.mu.Unlock()
		h.step()
	}
	if got := h.vitals(dead); got.Health != 0 {
		t.Errorf("a dead player regenerated to %d, want none at all", got.Health)
	}
}

// Hunger is paid only by connected, living ticks. The full budget is asserted rather
// than extrapolated from one point: one hundred intervals are exactly twelve hours.
func TestHungerDrainsFromFullToEmptyInTwelveHoursOfLivingPlay(t *testing.T) {
	t.Parallel()

	const rate = uint8(1)
	h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	if got, want := h.sim.hungerDrainTicks, ticksFor(HungerDrainInterval, rate); got != want {
		t.Fatalf("hunger interval = %d ticks, want %d", got, want)
	}
	total := int(PlayerMaxHunger) * int(h.sim.hungerDrainTicks)

	h.sim.mu.Lock()
	for range total - 1 {
		player.advanceVitalsLocked()
	}
	if player.hunger != 1 {
		t.Fatalf("one tick before the twelve-hour budget hunger = %d, want 1", player.hunger)
	}
	player.advanceVitalsLocked()
	if player.hunger != 0 {
		t.Errorf("after the twelve-hour budget hunger = %d, want 0", player.hunger)
	}
	if elapsed := time.Duration(total) * time.Second / time.Duration(rate); elapsed != 12*time.Hour {
		t.Errorf("full-to-empty budget = %v, want 12h", elapsed)
	}

	// A corpse pauses rather than resets the connected-play clock. One dead tick at
	// the edge of an interval must neither drain nor advance it.
	player.hunger = 1
	player.hungerTicks = h.sim.hungerDrainTicks - 1
	player.damageLocked(PlayerMaxHealth)
	before := player.hungerTicks
	player.advanceVitalsLocked()
	if player.hunger != 1 || player.hungerTicks != before {
		t.Errorf("a dead tick changed hunger to %d or its clock to %d; want 1 and %d",
			player.hunger, player.hungerTicks, before)
	}
	h.sim.mu.Unlock()
}

// Zero is the exact boundary of the movement penalty: one remaining point preserves
// the ordinary walk, while an empty reserve scales both horizontal axes and nothing
// vertical. A diagonal intent exercises both assignments in Player.step; the held
// jump makes the equal vertical result observable at the same time.
func TestOnlyAnEmptyStomachSlowsTheHorizontalWalk(t *testing.T) {
	t.Parallel()

	terrain := dropTerrain{groundTop: 63}
	velocityAt := func(hunger uint16) [3]float64 {
		h := newVitalsHarness(t, DefaultTickRate, terrain)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		player.hunger = hunger
		player.current = intent{moveX: 0.6, moveZ: 0.8, jump: true}
		player.onGround = true
		player.step(1/float64(DefaultTickRate), terrain)
		return player.vel
	}

	fed := velocityAt(1)
	starving := velocityAt(0)
	if got := math.Hypot(fed[0], fed[2]); math.Abs(got-WalkSpeed) > 1e-12 {
		t.Errorf("fed horizontal speed = %v, want %v", got, WalkSpeed)
	}
	if got, want := math.Hypot(starving[0], starving[2]), WalkSpeed*StarvingSpeedScale; math.Abs(got-want) > 1e-12 {
		t.Errorf("starving horizontal speed = %v, want %v", got, want)
	}
	if starving[1] != fed[1] {
		t.Errorf("starvation changed vertical velocity from %v to %v", fed[1], starving[1])
	}
}

// A leaving body stays alive in Sim for the server-owned linger, including after EOF.
// Those ticks are no longer connected play, so neither ordinary drain nor regeneration
// may change the stored life even when both are one tick from spending hunger.
func TestLeavingDoesNotAdvanceHungerOrRegeneration(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	player.health = 90
	player.hunger = 2
	player.hungerTicks = h.sim.hungerDrainTicks - 1
	player.sinceDamageTicks = h.sim.regenDelayTicks
	player.regenTicks = h.sim.regenIntervalTicks - 1
	player.regenPoints = HealthRegenPointsPerHunger - 1
	h.sim.mu.Unlock()
	player.BeginLeaving()

	h.sim.mu.Lock()
	beforeDrain, beforeRegen := player.hungerTicks, player.regenTicks
	player.advanceVitalsLocked()
	if player.health != 90 || player.hunger != 2 {
		t.Errorf("a leaving tick changed health/hunger to %d/%d, want 90/2",
			player.health, player.hunger)
	}
	if player.hungerTicks != beforeDrain || player.regenTicks != beforeRegen {
		t.Errorf("a leaving tick advanced drain/regen clocks to %d/%d, want %d/%d",
			player.hungerTicks, player.regenTicks, beforeDrain, beforeRegen)
	}
	h.sim.mu.Unlock()
}

func TestRegenerationSpendsOneHungerForEveryTwoHealth(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	player.health = 90
	player.hunger = 2
	player.sinceDamageTicks = h.sim.regenDelayTicks

	regeneratePoint := func() {
		player.regenTicks = h.sim.regenIntervalTicks - 1
		player.regenerateLocked()
	}
	regeneratePoint()
	if player.health != 91 || player.hunger != 2 {
		t.Fatalf("the first regenerated point left health/hunger %d/%d, want 91/2", player.health, player.hunger)
	}
	regeneratePoint()
	if player.health != 92 || player.hunger != 1 {
		t.Fatalf("the second regenerated point left health/hunger %d/%d, want 92/1", player.health, player.hunger)
	}
	regeneratePoint()
	regeneratePoint()
	if player.health != 94 || player.hunger != 0 {
		t.Fatalf("four regenerated points left health/hunger %d/%d, want 94/0", player.health, player.hunger)
	}
	regeneratePoint()
	if player.health != 94 {
		t.Errorf("zero hunger still regenerated health to %d", player.health)
	}
	h.sim.mu.Unlock()
}

// Damage resets the timing of regeneration, not what food has already paid for. If it
// reset the point counter too, repeatedly taking a hit after one healed point would make
// every recovery free.
func TestDamageCannotResetTheRegenerationHungerCost(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	player.health = 90
	player.hunger = 2
	player.sinceDamageTicks = h.sim.regenDelayTicks
	player.regenTicks = h.sim.regenIntervalTicks - 1
	player.regenerateLocked()
	player.damageLocked(1)
	player.sinceDamageTicks = h.sim.regenDelayTicks
	player.regenTicks = h.sim.regenIntervalTicks - 1
	player.regenerateLocked()
	if player.hunger != 1 {
		t.Errorf("two regenerated points separated by damage left hunger %d, want 1", player.hunger)
	}
	h.sim.mu.Unlock()
}

func TestRespawnRaisesHungerToItsFloorAndNeverLowersIt(t *testing.T) {
	t.Parallel()

	for _, start := range []uint16{10, 75} {
		t.Run(fmt.Sprintf("from %d", start), func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.sim.mu.Lock()
			player.hunger = start
			player.damageLocked(PlayerMaxHealth)
			h.sim.mu.Unlock()
			h.advance(int(h.sim.deathTicks))

			want := max(start, RespawnHungerFloor)
			h.sim.mu.Lock()
			got := player.hunger
			h.sim.mu.Unlock()
			if got != want {
				t.Errorf("respawn hunger = %d, want %d", got, want)
			}
		})
	}
}

// The two constants are the same wall-clock spans at every rate an operator may set.
//
// The same property #178 pinned for the hardness table, for the same reason: a delay
// written in ticks would be two seconds at 20 Hz and one at 40, and nothing about the game
// would look wrong — players would simply heal differently on different servers.
func TestRegenerationIsTheSameDurationAtEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{10, 20, 30, 64} {
		delay := ticksFor(HealthRegenDelay, rate)
		interval := ticksFor(HealthRegenInterval, rate)
		for _, c := range []struct {
			name  string
			ticks uint32
			want  time.Duration
		}{
			{"delay", delay, HealthRegenDelay},
			{"interval", interval, HealthRegenInterval},
		} {
			got := time.Duration(c.ticks) * time.Second / time.Duration(rate)
			if drift := got - c.want; drift > time.Second/time.Duration(rate) || drift < -time.Second/time.Duration(rate) {
				t.Errorf("%s is %v at %d Hz, want %v (drift %v, more than one tick)",
					c.name, got, rate, c.want, drift)
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Respawn — the settlement tier (#460)
// ---------------------------------------------------------------------------

// The capital of [testWorldSeed], stated here rather than looked up, so a change to the
// lattice that moves it turns these tests red instead of quietly re-aiming them.
const (
	testCapitalX       = 71
	testCapitalZ       = -142
	testCapitalPlateau = 75
)

// settlementRespawnGround is flat terrain to `top` with the two ways a column can be
// unusable spelled separately: voxels the server has not composed, and voxels something
// stands in.
//
// Both are needed because the respawn tier answers them the same way and for different
// reasons — a tick may not wait for a chunk, and a body may not start inside a solid —
// and a fixture that could only express one of them would let the other go untested.
type settlementRespawnGround struct {
	top     int64
	absent  map[[3]int64]bool
	blocked map[[3]int64]bool
}

func (w settlementRespawnGround) Block(x, y, z int64) (world.Block, bool) {
	at := [3]int64{x, y, z}
	if w.absent[at] {
		return world.Air, false
	}
	if w.blocked[at] {
		return world.Cobblestone, true
	}
	if y <= w.top {
		return world.Stone, true
	}
	return world.Air, true
}

// Solid is the production rule read from the palette, for the reason spawnGround's is.
func (w settlementRespawnGround) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w settlementRespawnGround) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// capitalGround is terrain whose surface is the capital's plateau, which is what the
// settlement tier is about to read.
func capitalGround() settlementRespawnGround {
	return settlementRespawnGround{
		top:     testCapitalPlateau,
		absent:  map[[3]int64]bool{},
		blocked: map[[3]int64]bool{},
	}
}

// respawnPositionOf is the policy under test, asked directly rather than through a
// death.
//
// Directly because the *height* is half of what the tier decides and a death does not
// preserve it: respawnLocked puts the body down with onGround false and the next tick
// settles it, so an end-to-end read answers where the fall stopped. The full cycle is
// exercised once, at the foot of this file.
func respawnPositionOf(h *vitalsHarness, p *Player, death [3]float64) [3]float64 {
	h.t.Helper()

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.pos = death
	return p.respawnPositionLocked()
}

// The tier this issue adds: no tent, so the body wakes at the nearest settlement rather
// than back at the world spawn a whole journey away.
//
// Three hundred blocks due south-east of the capital, because a bearing is what the
// offset is made of — see the diagonal note in the blocked-bearing test below.
func TestWithNoTentAPlayerWakesAtTheNearestSettlement(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 20, capitalGround())
	player, _ := h.join(1, [3]float32{0.5, testCapitalPlateau + 1, 0.5})

	centreX, centreZ := float64(testCapitalX)+0.5, float64(testCapitalZ)+0.5
	// Exactly on the diagonal, so the offset is respawnSettlementOffset/√2 on each axis.
	death := [3]float64{centreX + 300, testCapitalPlateau + 1, centreZ + 300}

	got := respawnPositionOf(h, player, death)

	step := respawnSettlementOffset / math.Sqrt2
	want := [3]float64{centreX + step, testCapitalPlateau + world.SpawnClearance, centreZ + step}
	for axis, w := range want {
		if math.Abs(got[axis]-w) > 1e-9 {
			t.Fatalf("respawn put the player at %v, want the capital's plateau at %v", got, want)
		}
	}
	if got == player.spawn {
		t.Fatalf("respawn used the world spawn %v, which is the tier this issue replaces", player.spawn)
	}
}

// The offset is what keeps two deaths off one voxel, and it is a bearing rather than a
// constant: the same settlement answers a different column for every direction somebody
// died in.
func TestTheSettlementRespawnPushesOutAlongTheBearingOfTheDeath(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 20, capitalGround())
	player, _ := h.join(1, [3]float32{0.5, testCapitalPlateau + 1, 0.5})

	centreX, centreZ := float64(testCapitalX)+0.5, float64(testCapitalZ)+0.5
	seen := map[[2]float64]bool{}
	for _, bearing := range [][2]float64{{400, 400}, {-400, 400}, {400, -400}, {-260, 900}} {
		got := respawnPositionOf(h, player,
			[3]float64{centreX + bearing[0], testCapitalPlateau + 1, centreZ + bearing[1]})

		if reach := math.Hypot(got[0]-centreX, got[2]-centreZ); math.Abs(reach-respawnSettlementOffset) > 1e-9 {
			t.Errorf("dying at bearing %v woke the player %v blocks from the centre, want %d",
				bearing, reach, respawnSettlementOffset)
		}
		key := [2]float64{got[0], got[2]}
		if seen[key] {
			t.Errorf("bearing %v stacked on a column another death already used: %v", bearing, key)
		}
		seen[key] = true
	}

	// Dying on the centre column itself has no bearing to push out along, and the answer
	// is the centre rather than a division by zero.
	got := respawnPositionOf(h, player, [3]float64{centreX, testCapitalPlateau + 1, centreZ})
	if got[0] != centreX || got[2] != centreZ {
		t.Errorf("a death on the centre woke the player at %v, want the centre column", got)
	}
}

// A tent still wins, and the settlement tier is genuinely underneath it rather than
// beside it: this is the same world the test above wakes at the capital in.
func TestATentStillOutranksTheNearestSettlement(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 20, capitalGround())
	player, _ := h.join(1, [3]float32{0.5, testCapitalPlateau + 1, 0.5})

	h.sim.mu.Lock()
	tent := &structure{
		structureID: 900,
		kind:        vnet.StructureKindTent,
		owner:       player.playerID,
		anchor:      [3]int32{20, testCapitalPlateau, 20},
	}
	h.sim.structures = map[uint64]*structure{tent.structureID: tent}
	h.sim.mu.Unlock()

	got := respawnPositionOf(h, player,
		[3]float64{float64(testCapitalX) + 300.5, testCapitalPlateau + 1, float64(testCapitalZ) + 300.5})

	want := [3]float64{20.5, testCapitalPlateau + 1, 20.5}
	if got != want {
		t.Fatalf("with a tent standing the player woke at %v, want the tent at %v", got, want)
	}
}

// The world spawn is still the floor under both tiers, and this is the case that reaches
// it honestly: a column in a world whose lattice holds no settlement within the bound
// [world.NearestSettlement] searches.
//
// Its own seed, because "nowhere near a settlement" is a property of a world rather than
// of a position — every seed puts a capital at spawn — and this pair was found by
// sweeping rather than asserted from the placement rules.
func TestWithNoSettlementInRangeTheWorldSpawnIsStillTheAnswer(t *testing.T) {
	t.Parallel()

	const (
		emptySeed = 318
		emptyX    = 40000
		emptyZ    = 40000
	)
	if _, found := world.NearestSettlement(emptySeed, emptyX, emptyZ); found {
		t.Fatalf("seed %d now holds a settlement near (%d, %d); this test needs a column that has none",
			emptySeed, emptyX, emptyZ)
	}

	h := newVitalsHarnessOver(t, 20, capitalGround(), 8, emptySeed)
	joined := [3]float32{emptyX + 0.5, testCapitalPlateau + 1, emptyZ + 0.5}
	player, _ := h.join(1, joined)

	got := respawnPositionOf(h, player,
		[3]float64{emptyX + 40.5, testCapitalPlateau + 1, emptyZ + 40.5})

	if got != player.spawn {
		t.Fatalf("with no settlement in range the player woke at %v, want the world spawn %v", got, player.spawn)
	}
}

// The verification the tier does on the column it picked, in both directions the
// non-generating read can refuse it.
func TestASettlementColumnTheTickCannotUseFallsThroughToTheWorldSpawn(t *testing.T) {
	t.Parallel()

	centreX, centreZ := float64(testCapitalX)+0.5, float64(testCapitalZ)+0.5
	// Due east, so the body stands in the single column at x = centre + 3. That is also
	// the bearing the capital's keep really blocks — see the schematic test below.
	death := [3]float64{centreX + 300, testCapitalPlateau + 1, centreZ}
	bed := [3]int64{testCapitalX + respawnSettlementOffset, 0, testCapitalZ}

	for _, tc := range []struct {
		name   string
		ground func() settlementRespawnGround
	}{
		{
			// The chunk has not been composed. A tick may not wait for one, and an
			// absent chunk is not somewhere to put a body.
			name: "the column is not loaded",
			ground: func() settlementRespawnGround {
				w := capitalGround()
				w.absent[[3]int64{bed[0], testCapitalPlateau + world.SpawnClearance, bed[2]}] = true
				return w
			},
		},
		{
			// Something stands in it. moveAndCollide refuses to move a body that starts
			// inside a solid, so this is the case where waking in the village would mean
			// waking inside its masonry.
			name: "something stands in the column",
			ground: func() settlementRespawnGround {
				w := capitalGround()
				w.blocked[[3]int64{bed[0], testCapitalPlateau + world.SpawnClearance, bed[2]}] = true
				return w
			},
		},
		{
			// The floor voxel itself has not been composed.
			name: "the plateau under it is not loaded",
			ground: func() settlementRespawnGround {
				w := capitalGround()
				w.absent[[3]int64{bed[0], testCapitalPlateau, bed[2]}] = true
				return w
			},
		},
		{
			// The floor is loaded and is air: the lattice says there is a plateau here
			// and the terrain does not have one. Answered like the rest, because a body
			// put down over nothing is a body that falls out of the village.
			name: "there is no plateau under it at all",
			ground: func() settlementRespawnGround {
				w := capitalGround()
				w.top = testCapitalPlateau - 1
				return w
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, 20, tc.ground())
			player, _ := h.join(1, [3]float32{0.5, testCapitalPlateau + 1, 0.5})

			// The same world with the same column left alone wakes them at the capital,
			// so the fallback below is this one voxel and not the fixture.
			clear := newVitalsHarness(t, 20, capitalGround())
			reference, _ := clear.join(1, [3]float32{0.5, testCapitalPlateau + 1, 0.5})
			if at := respawnPositionOf(clear, reference, death); at == reference.spawn {
				t.Fatalf("the unmodified fixture already fell through to the world spawn at %v", at)
			}

			if got := respawnPositionOf(h, player, death); got != player.spawn {
				t.Fatalf("the player woke at %v, want the world spawn %v", got, player.spawn)
			}
		})
	}
}

// settlementRespawnLocked makes a claim about a drawing, and a decision was made from it
// — that the offset stays at three blocks. So the drawing is asked.
//
// **The claim inverted at #555 and the decision did not.** Through worldgen 7 a capital
// blocked three of its four cardinals; the castle that replaced the keep is eleven
// blocks of open ground floor across, so every bearing is clear. What this test is for
// is the same either way: the offset is a constant chosen against a picture, and a
// picture is free to move under it.
//
// The keep stands unrotated on the settlement's centre and its floor is one above the
// plateau, so a body put down at Plateau + [world.SpawnClearance] occupies the drawing's
// y = 1 and y = 2.
func TestTheKeepStandsWhereThisRespawnRuleSaysItDoes(t *testing.T) {
	t.Parallel()

	keep := world.SchematicFor(world.BuildingKeep)
	midX, midZ := keep.W/2, keep.D/2

	for _, tc := range []struct {
		name   string
		dx, dz int
	}{
		{"east", respawnSettlementOffset, 0},
		{"west", -respawnSettlementOffset, 0},
		{"north", 0, -respawnSettlementOffset},
		{"south", 0, respawnSettlementOffset},
	} {
		for _, y := range []int{1, 2} {
			if got := keep.At(midX+tc.dx, y, midZ+tc.dz); got != world.Air {
				t.Errorf("%s of the castle's middle holds %v at y=%d; the respawn tier puts a body there",
					tc.name, got, y)
			}
		}
	}

	// The two smaller public buildings are the ones a village puts at its centre, and
	// the tier works on every bearing there because they are hollow this far out.
	for _, kind := range []world.BuildingKind{world.BuildingHall, world.BuildingSmithy} {
		drawing := world.SchematicFor(kind)
		midX, midZ := drawing.W/2, drawing.D/2
		for _, off := range [][2]int{
			{respawnSettlementOffset, 0}, {-respawnSettlementOffset, 0},
			{0, respawnSettlementOffset}, {0, -respawnSettlementOffset},
		} {
			for _, y := range []int{1, 2} {
				if got := drawing.At(midX+off[0], y, midZ+off[1]); got != world.Air {
					t.Errorf("%v holds %v at %v, y=%d — a village centre must be clear this far out",
						kind, got, off, y)
				}
			}
		}
	}
}

// Once through the real thing: a death, the countdown, and the tick that settles the
// body — so the tier is reached from where the simulation actually calls it and not only
// from a test's direct call.
func TestADeathWithNoTentEndsAtTheNearestSettlement(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 20, capitalGround())
	centreX, centreZ := float64(testCapitalX)+0.5, float64(testCapitalZ)+0.5
	player, _ := h.join(1, [3]float32{float32(centreX + 300), testCapitalPlateau + 1, float32(centreZ + 300)})

	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()
	h.advance(int(h.sim.deathTicks) + 1)

	step := respawnSettlementOffset / math.Sqrt2
	h.sim.mu.Lock()
	got := player.pos
	alive := player.alive()
	h.sim.mu.Unlock()

	if !alive {
		t.Fatalf("the player is still dead after the countdown")
	}
	// The column rather than the exact position: the respawn puts the body above the
	// ground with onGround false and the tick settles it, so the height that survives is
	// the collision skin's answer and the policy is what this test is about.
	if math.Abs(got[0]-(centreX+step)) > 1e-9 || math.Abs(got[2]-(centreZ+step)) > 1e-9 {
		t.Fatalf("the player came back at %v, want the capital's column near (%v, %v)",
			got, centreX+step, centreZ+step)
	}
}
