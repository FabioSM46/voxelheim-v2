package game

import (
	"sync"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The mob state machine, driven by ticks against fake terrain.
//
// Every assertion below is about what the *server* decided. The client draws a
// telegraph and a chase; whether either happened is this file's question.
//
// **One state machine, and the tests that name a draugr are testing the machine through
// the species it was written against.** What is species-specific — that a vargr outruns
// a walking player and a draugr does not, that the two bodies are reached differently by
// the same swing — is at the bottom of this file, and everything it asserts comes from
// mobRegistry rather than from a number written here.

// stepTerrain is flat ground with a raised shelf from shelfFromX onwards, which is how a
// one-block step and a three-block wall become the same test with one number changed.
type stepTerrain struct {
	groundTop  int64
	shelfFromX int64
	shelfTop   int64
}

func (w stepTerrain) Block(x, y, _ int64) (world.Block, bool) {
	top := w.groundTop
	if x >= w.shelfFromX {
		top = w.shelfTop
	}
	if y <= top {
		return world.Stone, true
	}
	return world.Air, true
}

func (w stepTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// mob is one live creature, read under the lock that owns it.
func (h *vitalsHarness) mob(entityID uint64) *mob {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.sim.mobs[entityID]
}

func (h *vitalsHarness) mobCount() int {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return len(h.sim.mobs)
}

// mobState is a copy of one mob's state, taken under the lock.
func (h *vitalsHarness) mobState(entityID uint64) (mob, bool) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m, ok := h.sim.mobs[entityID]
	if !ok {
		return mob{}, false
	}
	return *m, true
}

// spawnMobAtLocked places one creature of the named species, and fails the test rather
// than returning a zero identity when the registry refuses the kind.
//
// The caller holds Sim.mu.
func (h *vitalsHarness) spawnMobAtLocked(kind vnet.MobKind, pos [3]float64) uint64 {
	h.t.Helper()
	id, made := h.sim.spawnMobLocked(kind, pos)
	if !made {
		h.t.Fatalf("the simulation refused to place a %s", kind)
	}
	return id
}

// spawnMobAt puts one creature of the named species in the world and returns its
// identity; spawnDraugrAt is the draugr, which most of this file is written against.
//
// **There is no exported placement any more, deliberately**, so these reach for the same
// locked helper the spawn director uses. These tests are about what a creature does once
// it exists and about what a swing is worth against one; where the dark chooses to put
// them is spawn_test.go's question, and bending either file to the other's needs is how
// a test stops discriminating.
func (h *vitalsHarness) spawnDraugrAt(pos [3]float32) uint64 {
	h.t.Helper()
	return h.spawnMobAt(vnet.MobKindDraugr, pos)
}

func (h *vitalsHarness) spawnMobAt(kind vnet.MobKind, pos [3]float32) uint64 {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return h.spawnMobAtLocked(kind, [3]float64{float64(pos[0]), float64(pos[1]), float64(pos[2])})
}

// keepNight stops the world at the first tick of night, so a draugr with nothing to hunt
// survives the ticks a test wants to run.
//
// **Every test below that steps a target-less draugr needs this**, and that is the
// feature rather than an inconvenience: daylight removes one, which is the whole of the
// nocturnal rule. The clock is set rather than advanced because RestoreClock is the one
// way a tick of the day is chosen from outside the tick, and nothing here wants to
// simulate twelve minutes to reach dusk.
func (h *vitalsHarness) keepNight() {
	h.t.Helper()
	if err := h.sim.RestoreClock(NightStartTicks); err != nil {
		h.t.Fatalf("RestoreClock: %v", err)
	}
}

// ---------------------------------------------------------------------------
// Placement and identity
// ---------------------------------------------------------------------------

func TestADraugrArrivesIdleAtFullHealth(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{12.5, 64, 0.5})

	m, ok := h.mobState(id)
	if !ok {
		t.Fatal("the draugr was not created")
	}
	if m.kind != vnet.MobKindDraugr {
		t.Errorf("kind = %s, want Draugr", m.kind)
	}
	if m.health != draugrRow.maxHealth {
		t.Errorf("health = %d, want %d", m.health, draugrRow.maxHealth)
	}
	if m.action != vnet.MobActionIdle {
		t.Errorf("action = %s, want Idle", m.action)
	}
	if m.pos != ([3]float64{12.5, 64, 0.5}) {
		t.Errorf("pos = %v, want where it was placed", m.pos)
	}
}

// One counter for every entity, so no identity ever names two things.
func TestADraugrIdentityComesFromTheCounterThatNamesPlayers(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	first := h.spawnDraugrAt([3]float32{12.5, 64, 0.5})
	drop, ok := h.sim.spawnDrop(ItemStone, 1, [3]int64{4, 64, 0})
	if !ok {
		t.Fatal("the drop was refused")
	}

	if first == player.entityID || first == drop {
		t.Errorf("ids collide: player %d, draugr %d, drop %d", player.entityID, first, drop)
	}
}

// ---------------------------------------------------------------------------
// Choosing a target
// ---------------------------------------------------------------------------

func TestADraugrHuntsTheNearestPlayerAndBreaksTiesByIdentity(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})

	far, _ := h.join(7, [3]float32{10.5, 64, 0.5})
	near, _ := h.join(9, [3]float32{5.5, 64, 0.5})
	h.step()

	if got := h.mob(id); got.target != near.entityID {
		t.Errorf("target = %d, want the nearer player %d", got.target, near.entityID)
	}

	// Equal distances resolve by identity, and the lower one wins because the tick steps
	// players in identity order and the comparison below is strict.
	// Symmetric about the draugr, so the two distances are equal rather than merely
	// close — which one a tie picks is the whole point. The draugr is put back too: it
	// chased on the tick above, so leaving it where it ended would make this a test of
	// whichever player it had already started walking towards.
	h.sim.mu.Lock()
	h.sim.mobs[id].pos = [3]float64{0.5, 64, 0.5}
	far.pos = [3]float64{-4.5, 64, 0.5}
	near.pos = [3]float64{5.5, 64, 0.5}
	h.sim.mu.Unlock()
	h.step()

	if got := h.mob(id); got.target != far.entityID {
		t.Errorf("a tie chose %d, want the lower identity %d", got.target, far.entityID)
	}
}

func TestADraugrIgnoresWhatItCannotHunt(t *testing.T) {
	t.Parallel()

	for name, prepare := range map[string]func(*vitalsHarness, *Player){
		"a player beyond the aggro range": func(h *vitalsHarness, p *Player) {
			h.sim.mu.Lock()
			p.pos = [3]float64{draugrRow.aggroRange + 10, 64, 0.5}
			h.sim.mu.Unlock()
		},
		"a corpse": func(h *vitalsHarness, p *Player) {
			h.hurt(p, PlayerMaxHealth)
		},
		"a player under respawn protection": func(h *vitalsHarness, p *Player) {
			h.hurt(p, PlayerMaxHealth)
			h.advance(int(h.sim.deathTicks))
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			// Night, because every case here ends with a draugr that has nothing to
			// hunt — which is exactly what the daylight rule removes. "It chose
			// nobody" and "it is no longer here" are different answers and this test
			// is about the first.
			h.keepNight()
			id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
			player, _ := h.join(1, [3]float32{3.5, 64, 0.5})
			prepare(h, player)
			h.step()

			m := h.mob(id)
			if m.target != 0 {
				t.Errorf("the draugr is hunting %d", m.target)
			}
			if m.action != vnet.MobActionIdle {
				t.Errorf("action = %s, want Idle with nobody to hunt", m.action)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Chasing
// ---------------------------------------------------------------------------

func TestADraugrClosesTheDistanceToItsTarget(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{10.5, 64, 0.5})

	h.step()
	if got := h.mob(id).action; got != vnet.MobActionChase {
		t.Fatalf("action = %s, want Chase", got)
	}

	before := h.mob(id).pos
	h.advance(10)
	after := h.mob(id).pos

	if after[0] <= before[0] {
		t.Errorf("the draugr moved from x=%v to x=%v, away from its target", before[0], after[0])
	}
	// Straight at it: nothing here navigates, so the crossing axis must not drift.
	if diff := after[2] - before[2]; diff > 1e-6 || diff < -1e-6 {
		t.Errorf("the draugr drifted %v on z while walking along x", diff)
	}
	_ = player
}

func TestADeerFleesDirectlyAwayAndStopsBeyondItsReleaseRange(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnMobAt(vnet.MobKindDeer, [3]float32{0.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{10.5, 64, 0.5})

	h.step()
	fleeing := h.mob(id)
	if fleeing.action != vnet.MobActionFlee {
		t.Fatalf("action = %s, want Flee inside the deer's awareness", fleeing.action)
	}
	if fleeing.vel[0] >= 0 || fleeing.vel[2] != 0 {
		t.Errorf("velocity = %v, want directly away from the player on -X", fleeing.vel)
	}
	if fleeing.target != 0 {
		t.Errorf("a passive deer stored hostile target %d", fleeing.target)
	}

	h.sim.mu.Lock()
	player.pos = [3]float64{passiveFleeReleaseRange + 30, 64, 0.5}
	h.sim.mu.Unlock()
	h.step()
	stopped := h.mob(id)
	if stopped.action != vnet.MobActionIdle || stopped.vel[0] != 0 || stopped.vel[2] != 0 {
		t.Errorf("deer beyond release range is action=%s velocity=%v, want stationary Idle",
			stopped.action, stopped.vel)
	}
}

func TestDamageStartsADeerFleeingWithoutGivingItAnAttack(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnMobAt(vnet.MobKindDeer, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	h.sim.damageMobLocked(m, 1)
	damaged := *m
	h.sim.mu.Unlock()
	if damaged.action != vnet.MobActionFlee {
		t.Fatalf("damage left the deer in %s, want Flee", damaged.action)
	}

	// Even a corrupt prior combat state is routed back through the passive branch rather
	// than allowed to count down or land an attack.
	for _, forbidden := range []vnet.MobAction{vnet.MobActionWindup, vnet.MobActionRecovery} {
		h.sim.mu.Lock()
		h.sim.mobs[id].action = forbidden
		h.sim.mobs[id].actionTicks = 1
		h.sim.mu.Unlock()
		h.step()
		if got := h.mob(id).action; got == vnet.MobActionWindup || got == vnet.MobActionRecovery {
			t.Errorf("passive deer remained in attack state %s", got)
		}
	}
}

// One block is what a player steps over without thinking, so it is what a draugr clears.
// Anything taller is a wall it is allowed to be stuck behind — pathfinding is a separate
// system and this one must not quietly contain half of it.
func TestADraugrHopsAOneBlockStepAndIsStoppedByAWall(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		terrain dropTerrain
		wallTop int64
		crosses bool
	}{
		"a one-block step":   {wallTop: 64, crosses: true},
		"a three-block wall": {wallTop: 66, crosses: false},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			// Ground at 63, and a raised shelf from x >= 4.
			top := tc.wallTop
			terrain := stepTerrain{groundTop: 63, shelfFromX: 4, shelfTop: top}
			h := newVitalsHarness(t, DefaultTickRate, terrain)
			id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
			h.join(1, [3]float32{9.5, float32(top + 1), 0.5})

			h.advance(120)
			m := h.mob(id)

			if tc.crosses && m.pos[0] < 4 {
				t.Errorf("the draugr stopped at x=%v and never climbed the step", m.pos[0])
			}
			if !tc.crosses && m.pos[0] >= 4 {
				t.Errorf("the draugr reached x=%v, through a wall it should not clear", m.pos[0])
			}
			// Either way it must not be inside the terrain.
			if overlaps(terrain, draugrRow.body.boxAt(m.pos)) {
				t.Errorf("the draugr ended inside solid terrain at %v", m.pos)
			}
		})
	}
}

// An absent chunk is solid, and the tick never generates one to find out.
func TestUnloadedTerrainIsSolidToADraugr(t *testing.T) {
	t.Parallel()

	// Everything past x = 4 is a chunk that has not arrived.
	terrain := dropTerrain{
		groundTop: 63,
		absent:    func(x, _, _ int64) bool { return x >= 4 },
	}
	h := newVitalsHarness(t, DefaultTickRate, terrain)
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	h.join(1, [3]float32{9.5, 64, 0.5})

	h.advance(120)
	m := h.mob(id)

	if m.pos[0] >= 4 {
		t.Errorf("the draugr walked to x=%v, into terrain that has not arrived", m.pos[0])
	}
	for axis, v := range m.vel {
		if v != v {
			t.Errorf("velocity axis %d is not a number", axis)
		}
	}
}

// ---------------------------------------------------------------------------
// Attacking
// ---------------------------------------------------------------------------

func TestADraugrTelegraphsBeforeItHits(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{1.5, 64, 0.5})

	h.step()
	if got := h.mob(id).action; got != vnet.MobActionWindup {
		t.Fatalf("action = %s, want Windup within reach", got)
	}
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Fatalf("the windup already cost %d health", PlayerMaxHealth-got)
	}

	// The whole telegraph passes before anything lands.
	for tick := range int(h.sim.mobTimings[vnet.MobKindDraugr].windup) {
		if got := h.vitals(player).Health; got != PlayerMaxHealth {
			t.Fatalf("the blow landed %d ticks into the telegraph", tick)
		}
		h.step()
	}

	if got := h.vitals(player).Health; got != PlayerMaxHealth-draugrRow.damage {
		t.Errorf("health is %d after one blow, want %d", got, PlayerMaxHealth-draugrRow.damage)
	}
	if got := h.mob(id).action; got != vnet.MobActionRecovery {
		t.Errorf("action = %s after a blow, want Recovery", got)
	}
}

func draugrBlowAgainst(t *testing.T, pieces ...testArmourPiece) uint16 {
	t.Helper()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	pos := [3]float32{1.5, 64, 0.5}
	life := lifeWearing(t, pos, pieces...)
	player, _ := h.joinLife(1, pos, &life)

	h.step()
	h.advance(int(h.sim.mobTimings[vnet.MobKindDraugr].windup))
	return PlayerMaxHealth - h.vitals(player).Health
}

func TestWornArmourSoftensADraugrBlow(t *testing.T) {
	leather := []testArmourPiece{
		fullTestArmour(ItemLeatherCap),
		fullTestArmour(ItemLeatherJerkin),
		fullTestArmour(ItemLeatherLeggings),
	}
	iron := []testArmourPiece{
		fullTestArmour(ItemIronHelm),
		fullTestArmour(ItemIronCuirass),
		fullTestArmour(ItemIronGreaves),
	}
	mixed := []testArmourPiece{
		fullTestArmour(ItemIronHelm),
		fullTestArmour(ItemLeatherJerkin),
		fullTestArmour(ItemIronGreaves),
	}
	wornThrough := []testArmourPiece{
		{item: ItemIronHelm, durability: 0},
		fullTestArmour(ItemIronCuirass),
		fullTestArmour(ItemIronGreaves),
	}
	mixedArmour := itemRegistry[ItemIronHelm].armour +
		itemRegistry[ItemLeatherJerkin].armour +
		itemRegistry[ItemIronGreaves].armour

	for _, tc := range []struct {
		name   string
		pieces []testArmourPiece
		want   uint16
	}{
		{name: "full leather", pieces: leather, want: 8},
		{name: "full iron", pieces: iron, want: 7},
		{name: "mixed", pieces: mixed, want: uint16(uint32(draugrRow.damage) * uint32(ArmourScale-mixedArmour) / uint32(ArmourScale))},
		{name: "one iron piece worn through", pieces: wornThrough, want: 8},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := draugrBlowAgainst(t, tc.pieces...); got != tc.want {
				t.Errorf("the blow cost %d health, want %d", got, tc.want)
			}
		})
	}
}

func TestEvenCompleteArmourCannotEraseABlow(t *testing.T) {
	const completeArmour ItemID = 64_970
	itemRegistry[completeArmour] = itemDefinition{
		places: world.Air, maxStack: 1, wornAt: wornHead, armour: ArmourScale, maxDurability: 1,
	}
	t.Cleanup(func() { delete(itemRegistry, completeArmour) })

	if got := draugrBlowAgainst(t, fullTestArmour(completeArmour)); got != 1 {
		t.Errorf("a blow against 100%% test armour cost %d health, want the floor of 1", got)
	}
}

func TestAnArmourReducedBlowStillRestartsRegeneration(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	pos := [3]float32{1.5, 64, 0.5}
	life := lifeWearing(t, pos,
		fullTestArmour(ItemIronHelm),
		fullTestArmour(ItemIronCuirass),
		fullTestArmour(ItemIronGreaves),
	)
	player, _ := h.joinLife(1, pos, &life)
	h.step()

	h.sim.mu.Lock()
	player.sinceDamageTicks = h.sim.regenDelayTicks
	player.regenTicks = h.sim.regenIntervalTicks - 1
	h.sim.mu.Unlock()
	h.advance(int(h.sim.mobTimings[vnet.MobKindDraugr].windup))

	h.sim.mu.Lock()
	sinceDamage, regen := player.sinceDamageTicks, player.regenTicks
	h.sim.mu.Unlock()
	if sinceDamage != 0 || regen != 0 {
		t.Errorf("the reduced blow left regeneration clocks at since=%d regen=%d, want both zero", sinceDamage, regen)
	}
}

// Leaving reach mid-telegraph costs the draugr its swing — and costs it no recovery,
// because recovery is what an attack pays and this was not one.
func TestASwingAbandonedMidTelegraphLandsNothing(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{1.5, 64, 0.5})

	h.step()
	if got := h.mob(id).action; got != vnet.MobActionWindup {
		t.Fatalf("action = %s, want Windup", got)
	}

	h.sim.mu.Lock()
	player.pos = [3]float64{12.5, 64, 0.5}
	h.sim.mu.Unlock()

	// The whole telegraph, not one tick of it. A single step cannot tell an abandoned
	// swing from one that is merely still counting down — both cost nothing yet.
	h.advance(int(h.sim.mobTimings[vnet.MobKindDraugr].windup) + 2)

	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("an abandoned swing cost %d health", PlayerMaxHealth-got)
	}
	if got := h.mob(id).action; got == vnet.MobActionRecovery {
		t.Error("an abandoned swing paid recovery; recovery is what an attack costs")
	}
	// Twelve blocks is out of reach and the draugr closes at draugrRow.speed, so this is a
	// chase rather than a second swing.
	if got := h.mob(id).action; got != vnet.MobActionChase {
		t.Errorf("action = %s after the target left, want Chase", got)
	}
}

// The cadence is the server's, at any rate Step is called at.
func TestTheAttackCadenceIsTheSameAtEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{5, 20, 60} {
		t.Run("tick rate "+string(rune('0'+rate%10)), func(t *testing.T) {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
			player, _ := h.join(1, [3]float32{1.5, 64, 0.5})

			// One second of contact, at whatever resolution this rate gives it.
			h.advance(int(rate))

			// A blow costs the draugr's damage and a cycle is windup plus recovery, so the
			// number of blows in a second is bounded by the cycle and not by the tick
			// count. The exact figure is the same at every rate, which is the point.
			lost := PlayerMaxHealth - h.vitals(player).Health
			blows := int(lost) / int(draugrRow.damage)
			if blows < 1 {
				t.Errorf("no blow landed in a second at %d Hz", rate)
			}
			if blows > 2 {
				t.Errorf("%d blows landed in a second at %d Hz; the cycle bounds it", blows, rate)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Dying
// ---------------------------------------------------------------------------

// A kill puts a draugr down, takes it away when its death is over, and schedules nothing.
//
// **The absence is the assertion.** Killing one used to start a countdown to a fresh
// draugr at the same anchor, which made a kill a way of *moving* a creature rather than
// of removing it — the same one came back, in the same field, however many times you
// went there. Nothing replaces it now; what refills the world is the director, and only
// where somebody is standing.
//
// **The removal is no longer the blow, and both halves are asserted.** A killed creature
// stays in the world, and in every snapshot, in MobActionDying with no health left; the
// body stops existing MobDeathDuration later. A test that only checked the end state would
// pass just as well against a server that deleted it on the instant, which is the design
// this replaced.
func TestAKilledDraugrGoesDownAndThenLeavesTheWorld(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	first := h.spawnDraugrAt([3]float32{12.5, 64, 0.5})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.step()

	h.sim.mu.Lock()
	h.sim.damageMobLocked(h.sim.mobs[first], draugrRow.maxHealth)
	h.sim.mu.Unlock()

	body, live := h.mobState(first)
	if !live {
		t.Fatal("a killed draugr left the world on the tick of the blow, with no death to watch")
	}
	if !body.dying() {
		t.Errorf("a killed draugr is in %s, want Dying", body.action)
	}
	if body.health != 0 {
		t.Errorf("a killed draugr has %d health, want none", body.health)
	}

	h.step()
	var shown bool
	for _, state := range newestSnapshotMobs(t, out) {
		if state.EntityID != first {
			continue
		}
		shown = true
		if state.Action != vnet.MobActionDying || state.Health != 0 {
			t.Errorf("the snapshot draws the body as %s with %d health, want Dying with none",
				state.Action, state.Health)
		}
	}
	if !shown {
		t.Error("a body going down is in no snapshot entry, so nothing can draw it falling")
	}
	// The wait is the server's, and nothing is on the ground until it is over.
	if got := h.sim.DropCount(); got != 0 {
		t.Errorf("a draugr still going down has already left %d drops", got)
	}

	h.advance(int(h.sim.mobDeathTicks) + 1)
	if _, live := h.mobState(first); live {
		t.Fatal("the body outlived the whole of its own death")
	}
	// By identity rather than by count: it is night with a player connected, so the
	// director has had two and a half seconds to put fresh creatures in that snapshot and
	// a total would be asserting the director's behaviour instead of this one's.
	for _, state := range newestSnapshotMobs(t, out) {
		if state.EntityID == first {
			t.Error("a body that stopped existing is still in the snapshot")
		}
	}

	// Nothing comes back at that spot. The director may put a creature *somewhere* —
	// it is night and a player is connected — but never at the place one died, which is
	// what the old countdown did and this test is the record of.
	h.advance(10 * DefaultTickRate)
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	for _, m := range h.sim.mobs {
		if m.pos == ([3]float64{12.5, 64, 0.5}) {
			t.Errorf("a draugr stands at the spot the dead one was killed on: %v", m.pos)
		}
	}
}

func TestDamagingADraugrShortOfDeathLeavesItHunting(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	h.sim.damageMobLocked(h.sim.mobs[id], draugrRow.maxHealth-1)
	h.sim.mu.Unlock()

	m, ok := h.mobState(id)
	if !ok {
		t.Fatal("a wounded draugr left the world")
	}
	if m.health != 1 {
		t.Errorf("health = %d, want 1", m.health)
	}

	// And zero damage is not a hit.
	h.sim.mu.Lock()
	h.sim.damageMobLocked(h.sim.mobs[id], 0)
	h.sim.mu.Unlock()
	if got, _ := h.mobState(id); got.health != 1 {
		t.Errorf("zero damage changed health to %d", got.health)
	}
}

// ---------------------------------------------------------------------------
// What a session is told
// ---------------------------------------------------------------------------

// newestSnapshotMobs is the mob vector of the newest snapshot this session was sent.
func newestSnapshotMobs(t *testing.T, out *dropSink) []protocol.MobState {
	t.Helper()

	snapshot := newestSnapshot(t, out)
	mobs := make([]protocol.MobState, 0, snapshot.MobsLength())
	for i := range snapshot.MobsLength() {
		var m vnet.MobState
		if !snapshot.Mobs(&m, i) {
			t.Fatalf("mob %d is missing from a snapshot that claims to hold it", i)
		}
		pos, vel := m.Pos(nil), m.Vel(nil)
		if pos == nil || vel == nil {
			t.Fatalf("mob %d carries no position or velocity", i)
		}
		mobs = append(mobs, protocol.MobState{
			EntityID:  m.EntityId(),
			Kind:      m.Kind(),
			Pos:       [3]float32{pos.X(), pos.Y(), pos.Z()},
			Vel:       [3]float32{vel.X(), vel.Y(), vel.Z()},
			Yaw:       m.Yaw(),
			Health:    m.Health(),
			MaxHealth: m.MaxHealth(),
			Action:    m.Action(),
		})
	}
	return mobs
}

// A mob beyond the view cube is a creature standing on terrain the client has never been
// sent, so it is not in the snapshot at all.
func TestOnlyMobsInsideTheViewCubeAreSent(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
	near := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.step()

	if got := newestSnapshotMobs(t, out); len(got) != 1 || got[0].EntityID != near {
		t.Fatalf("the snapshot holds %+v, want just the near draugr %d", got, near)
	}

	// Well past a view distance of one chunk.
	h.sim.mu.Lock()
	h.sim.mobs[near].pos = [3]float64{500.5, 64, 0.5}
	h.sim.mobs[near].chunk = chunkAt(h.sim.mobs[near].pos)
	h.sim.mu.Unlock()
	h.step()

	if got := newestSnapshotMobs(t, out); len(got) != 0 {
		t.Errorf("the snapshot holds %+v for a draugr outside the view cube", got)
	}
}

// Every value the contract says a decoder refuses, asserted on what the tick emits.
func TestTheMobsTheTickEmitsSatisfyTheContract(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.spawnDraugrAt([3]float32{2.5, 64, 0.5})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	h.advance(5)

	mobs := newestSnapshotMobs(t, out)
	if len(mobs) != 1 {
		t.Fatalf("the snapshot holds %d mobs, want 1", len(mobs))
	}
	m := mobs[0]
	if m.EntityID == 0 {
		t.Error("the mob carries the reserved entity id 0")
	}
	if m.Kind == vnet.MobKindUnknown {
		t.Error("the mob has an unknown kind")
	}
	if m.Action == vnet.MobActionUnknown {
		t.Error("the mob has an unknown action")
	}
	if m.MaxHealth == 0 || m.Health > m.MaxHealth {
		t.Errorf("the mob is %d/%d", m.Health, m.MaxHealth)
	}
	for axis := range 3 {
		if v := m.Pos[axis]; v != v {
			t.Errorf("pos axis %d is not a number", axis)
		}
		if v := m.Vel[axis]; v != v {
			t.Errorf("vel axis %d is not a number", axis)
		}
	}
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

// The tick steps mobs under Sim.mu while sessions read and write through the exported
// methods. This is the shape -race is here to judge.
func TestMobsUnderConcurrentSessionTraffic(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.spawnDraugrAt([3]float32{2.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	var wg sync.WaitGroup
	stop := make(chan struct{})
	wg.Add(1)
	go func() {
		defer wg.Done()
		var tick uint32
		for {
			select {
			case <-stop:
				return
			default:
			}
			tick++
			_ = player.Submit(protocol.PlayerInput{ClientTick: tick, MoveZ: 1, Yaw: 0.25})
			_ = player.InventoryState()
		}
	}()

	h.advance(200)
	close(stop)
	wg.Wait()

	// **What the world ends up holding is deliberately not asserted.** The draugr
	// chases a player who outruns it, loses the target, and the daylight takes it —
	// so a count here would be pinning an outcome this test is not about. What it is
	// about is that a session goroutine submitting input and reading the pack while
	// the tick holds the simulation's lock cannot tear anything: under -race that is
	// the detector's verdict, and without it a torn read surfaces as a coordinate that
	// is not a number.
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	for id, m := range h.sim.mobs {
		for axis, v := range m.pos {
			if v != v {
				t.Errorf("mob %d has a position axis %d that is not a number", id, axis)
			}
		}
	}
}

// The detection half of the step-up is the same at every tick rate the server accepts.
//
// It is asked of stepsUp directly rather than through a climb, because *whether the step
// is seen* and *whether the jump clears it* are different questions with different
// answers — see the test below for the second.
func TestAStepIsSeenAtEveryTickRate(t *testing.T) {
	t.Parallel()

	terrain := stepTerrain{groundTop: 63, shelfFromX: 4, shelfTop: 64}

	for _, rate := range []uint8{1, 2, 5, 10, 15, 20, 30, 60, 120, 255} {
		dt := 1.0 / float64(rate)
		heading := [2]float64{draugrRow.speed, 0}

		// Walk the whole approach and require the step to be seen before the body would
		// enter the column. A fixed probe window is a distance measured in blocks
		// against a stride measured in blocks per tick, so a coarse enough rate steps
		// clean over it — which is exactly what happened at 10 and 15 Hz.
		seen := false
		for pos := 0.5; pos+draugrRow.body.width/2 < 4; pos += draugrRow.speed * dt {
			m := &mob{kind: vnet.MobKindDraugr, pos: [3]float64{pos, 64.0001, 0.5}}
			if m.stepsUp(terrain, heading, dt) {
				seen = true
				break
			}
		}
		if !seen {
			t.Errorf("%d Hz never saw the step before reaching it", rate)
		}
	}
}

// And the climb itself, wherever the physics can deliver it.
//
// **Ten hertz is a floor the draugr does not own.** JumpImpulse under a fixed timestep
// loses height as the step coarsens — the apex is 1.230 blocks at 20 Hz, 1.020 at 10,
// 0.938 at 8 and 0.680 at 5 — so below about 10 Hz *nothing* clears a one-block step,
// a player included. That is the integrator's limit rather than this state machine's,
// and asserting a climb there would be asserting a physics this server does not have.
func TestADraugrClimbsAStepAtEveryRateThePhysicsAllows(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{10, 15, 20, 30, 60, 120, 255} {
		terrain := stepTerrain{groundTop: 63, shelfFromX: 4, shelfTop: 64}
		h := newVitalsHarness(t, rate, terrain)
		id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
		h.join(1, [3]float32{9.5, 65, 0.5})

		h.advance(int(rate) * 8)
		if got := h.mob(id).pos[0]; got < 4 {
			t.Errorf("%d Hz: the draugr stopped at x=%v and never climbed the step", rate, got)
		}
	}
}

// ---------------------------------------------------------------------------
// The collision edge this issue found
// ---------------------------------------------------------------------------

// A body that comes to rest exactly flush against a solid face must still be able to
// move afterwards.
//
// Half-open boxes make flush *legal*, so a sub-step can land the leading face on a voxel
// boundary with no collision detected — and therefore without the gap a detected
// collision leaves. One ulp of accumulated drift past that boundary then reads as an
// overlap, moveAndCollide answers "already inside something" by blocking every axis, and
// the body is immobile for the rest of its life. It cannot even rise out.
//
// The arithmetic to reach it is ordinary rather than adversarial: 0.16 blocks a tick from
// x = 0.5 lands at 4.000000000000001 against a face at 4. It applies to players exactly
// as it does to mobs — it lives here because a draugr climbing a step is the first thing
// that had to leave the state rather than merely stand in it.
func TestABodyRestingFlushAgainstAFaceCanStillMove(t *testing.T) {
	t.Parallel()

	// Ground at 63 with a wall from x >= 4.
	terrain := stepTerrain{groundTop: 63, shelfFromX: 4, shelfTop: 70}

	// Twenty ticks of exactly the step that lands the leading face on the boundary.
	pos := [3]float64{0.5, 64, 0.5}
	for range 20 {
		pos, _ = moveAndCollide(terrain, playerBody, pos, [3]float64{0.16, 0, 0})
	}

	if overlaps(terrain, playerBox(pos)) {
		t.Fatalf("the body came to rest overlapping the wall at %v", pos)
	}

	// The state that made it fatal: rising out of it.
	up, blocked := moveAndCollide(terrain, playerBody, pos, [3]float64{0, 0.4, 0})
	if blocked[1] || up[1] <= pos[1] {
		t.Errorf("a body flush against a wall could not rise: %v -> %v, blocked = %v", pos, up, blocked)
	}

	// And backing away from it.
	back, _ := moveAndCollide(terrain, playerBody, pos, [3]float64{-0.4, 0, 0})
	if back[0] >= pos[0] {
		t.Errorf("a body flush against a wall could not back away: %v -> %v", pos, back)
	}
}

// ---------------------------------------------------------------------------
// Two species, one state machine
// ---------------------------------------------------------------------------

// hold submits one movement intent every tick for n ticks, and steps between them.
//
// Resubmitted rather than sent once, the way a client does it: an accepted input is only
// good for half a second (see idleLimitTicks), so a test that sent one frame and stepped
// for three seconds would be measuring the idle rule instead of the chase.
func (h *vitalsHarness) hold(p *Player, in protocol.PlayerInput, ticks int) {
	h.t.Helper()
	for range ticks {
		in.ClientTick = uint32(h.tick) + 1
		if err := p.Submit(in); err != nil {
			h.t.Fatalf("Submit: %v", err)
		}
		h.step()
	}
}

// A vargr closes on a player who is running away; a draugr is left behind.
//
// **The one number the vargr is arranged around, asked as behaviour.** [WalkSpeed] is
// what a player gets at full intent, so a creature under it can be walked away from and
// a creature over it cannot — which is the difference between "turn and fight" being a
// choice and being the only move left. Both are the same state machine steering at the
// same target; only the row differs.
func TestAVargrRunsDownAWalkingPlayerAndADraugrDoesNot(t *testing.T) {
	t.Parallel()

	// Three seconds of running, which at a difference of about a block a second is long
	// enough for the gap to move unambiguously in one direction or the other.
	const seconds = 3

	chase := func(t *testing.T, kind vnet.MobKind) (before, after float64) {
		t.Helper()

		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		// Ten blocks behind the player, inside every registered aggro range, so the
		// chase is under way from the first tick.
		id := h.spawnMobAt(kind, [3]float32{0.5, 64, 10.5})

		// One tick to settle and to let the mob choose its target.
		h.step()
		gap := func() float64 {
			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			return h.sim.mobs[id].pos[2] - player.pos[2]
		}
		before = gap()

		// Yaw 0 looks along -Z, and MoveZ at full intent walks the way the player is
		// facing: straight away from the thing behind them.
		h.hold(player, protocol.PlayerInput{MoveZ: 1, Yaw: 0}, seconds*DefaultTickRate)
		return before, gap()
	}

	t.Run("vargr", func(t *testing.T) {
		t.Parallel()
		before, after := chase(t, vnet.MobKindVargr)
		if after >= before {
			t.Errorf("a vargr at %v blocks per second was %v blocks behind and is now %v, "+
				"against a walk of %v", vargrRow.speed, before, after, WalkSpeed)
		}
	})

	t.Run("draugr", func(t *testing.T) {
		t.Parallel()
		before, after := chase(t, vnet.MobKindDraugr)
		if after <= before {
			t.Errorf("a draugr at %v blocks per second was %v blocks behind and is now %v, "+
				"against a walk of %v: running away stopped being an answer to one",
				draugrRow.speed, before, after, WalkSpeed)
		}
	})
}

// The step probe reads the body it is probing for.
//
// stepsUp looks a body's own half-width past its leading face, so a wider creature has
// to look further ahead — and it checks the column is clear for that creature's own
// height, so a shorter one fits under a gap a taller one does not. Both come from the
// registry; a hardcoded box would answer the draugr's question for every species.
func TestTheStepProbeReadsTheProbingBodyFromTheRegistry(t *testing.T) {
	t.Parallel()

	// A one-block step with a ceiling two blocks over the ground: a vargr is a block
	// tall and fits on top of it, a draugr is 1.8 and does not.
	terrain := lowCeilingStep{groundTop: 63, stepFromX: 4, ceilingAt: 66}

	const dt = 1.0 / float64(DefaultTickRate)
	for kind, want := range map[vnet.MobKind]bool{vnet.MobKindVargr: true, vnet.MobKindDraugr: false} {
		def := mobRegistry[kind]
		m := &mob{kind: kind, pos: [3]float64{3.5, 64.0001, 0.5}}
		heading := [2]float64{def.speed, 0}
		if got := m.stepsUp(terrain, heading, dt); got != want {
			t.Errorf("%s (%v wide, %v tall) sees the step as %v, want %v",
				kind, def.body.width, def.body.height, got, want)
		}
	}
}

// lowCeilingStep is flat ground with a one-block step from stepFromX, under a solid
// ceiling at ceilingAt — so how much room there is above the step depends entirely on
// how tall the thing trying to climb it is.
type lowCeilingStep struct {
	groundTop int64
	stepFromX int64
	ceilingAt int64
}

func (w lowCeilingStep) Block(x, y, _ int64) (world.Block, bool) {
	top := w.groundTop
	if x >= w.stepFromX {
		top = w.groundTop + 1
	}
	if y <= top || y >= w.ceilingAt {
		return world.Stone, true
	}
	return world.Air, true
}

func (w lowCeilingStep) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// The wire form carries each species' own kind and its own maximum health.
//
// **The kind is what lets a client draw the right creature; the maximum is what its
// health bar is drawn against.** The maximum used to be one constant for every mob in
// the world, so a vargr at full health would have been sent as 35 out of 60 — three
// fifths of a bar, permanently, on a creature that had not been touched.
func TestTheWireFormCarriesEachSpeciesOwnKindAndMaximum(t *testing.T) {
	t.Parallel()

	states := mobStates([]*mob{
		{entityID: 7, kind: vnet.MobKindDraugr, health: draugrRow.maxHealth, action: vnet.MobActionIdle},
		{entityID: 9, kind: vnet.MobKindVargr, health: vargrRow.maxHealth, action: vnet.MobActionChase},
	})
	if len(states) != 2 {
		t.Fatalf("two mobs produced %d states", len(states))
	}

	want := map[uint64]struct {
		kind      vnet.MobKind
		maxHealth uint16
	}{
		7: {vnet.MobKindDraugr, draugrRow.maxHealth},
		9: {vnet.MobKindVargr, vargrRow.maxHealth},
	}
	for _, state := range states {
		expected := want[state.EntityID]
		if state.Kind != expected.kind {
			t.Errorf("mob %d went out as a %s, want a %s", state.EntityID, state.Kind, expected.kind)
		}
		if state.MaxHealth != expected.maxHealth {
			t.Errorf("mob %d went out with a maximum of %d, want its own %d",
				state.EntityID, state.MaxHealth, expected.maxHealth)
		}
		if state.Health != state.MaxHealth {
			t.Errorf("mob %d is %d/%d, and it was created at full health",
				state.EntityID, state.Health, state.MaxHealth)
		}
	}
	if draugrRow.maxHealth == vargrRow.maxHealth {
		t.Error("both species have the same maximum, so this test could not tell one answer from two")
	}
}
