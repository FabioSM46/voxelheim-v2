package game

import (
	"math"
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The descent's two lesser species: their rows, the shell, the two-attack rhythm, the
// burial and the leash. Everything is asked of the server's state; what a client draws
// from it is the client's question.

var (
	caveSpiderRow = mobRegistry[vnet.MobKindCaveSpider]
	scorpionRow   = mobRegistry[vnet.MobKindScorpion]
)

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

// Both rows, pinned in full, on the draugr's terms: a rebalance is a decision that has to
// be taken here as well as in species.go, and a field added to mobDefinition and left out
// here is a compile error rather than a silent zero.
func TestTheDescentRowsCarryThePricedNumbers(t *testing.T) {
	t.Parallel()

	wantSpider := mobDefinition{
		rank:        mobRankNormal,
		maxHealth:   20,
		experience:  6,
		speed:       5.0,
		aggroRange:  14.0,
		attackRange: 1.4,
		damage:      4,
		windup:      300 * time.Millisecond,
		recovery:    600 * time.Millisecond,
		body:        body{width: 0.9, height: 0.6},
		loot:        []lootRoll{{item: ItemBone, min: 1, max: 1}},
		dungeonOnly: true,
	}
	wantScorpion := mobDefinition{
		rank:        mobRankNormal,
		maxHealth:   72,
		experience:  25,
		speed:       2.6,
		aggroRange:  10.0,
		attackRange: 2.2,
		damage:      18,
		windup:      1100 * time.Millisecond,
		recovery:    1400 * time.Millisecond,
		body:        body{width: 1.3, height: 0.6},
		loot:        []lootRoll{{item: ItemBone, min: 1, max: 1}},
		armour:      40,
		swipe: mobAttack{
			reach: 1.5, damage: 7, windup: 450 * time.Millisecond, recovery: 700 * time.Millisecond,
		},
		emergeRange: 5.0,
		emergence:   800 * time.Millisecond,
		dungeonOnly: true,
	}
	if !reflect.DeepEqual(caveSpiderRow, wantSpider) {
		t.Errorf("the cave spider's row is %+v, want %+v", caveSpiderRow, wantSpider)
	}
	if !reflect.DeepEqual(scorpionRow, wantScorpion) {
		t.Errorf("the scorpion's row is %+v, want %+v", scorpionRow, wantScorpion)
	}
	// The ordinary minor loot table is one table, shared by name.
	if !reflect.DeepEqual(caveSpiderRow.loot, minorLoot) || !reflect.DeepEqual(scorpionRow.loot, minorLoot) {
		t.Error("a descent row does not carry the ordinary minor loot table")
	}
}

// The spider is the swarm: faster than a walk, dead to any one blade, and it bites fast.
func TestACaveSpiderOutrunsAWalkAndDiesToOneBlade(t *testing.T) {
	t.Parallel()

	if caveSpiderRow.speed <= WalkSpeed {
		t.Errorf("a cave spider closes at %v against a walk of %v: a wave could be walked away from",
			caveSpiderRow.speed, WalkSpeed)
	}
	if caveSpiderRow.speed >= vargrRow.speed {
		t.Errorf("a cave spider at %v is not slower than the vargr's %v", caveSpiderRow.speed, vargrRow.speed)
	}
	if caveSpiderRow.maxHealth > RustySwordDamage {
		t.Errorf("a cave spider has %d health, more than one rusty swing's %d", caveSpiderRow.maxHealth, RustySwordDamage)
	}
	for kind, def := range mobRegistry {
		if kind == vnet.MobKindCaveSpider || def.passive {
			continue
		}
		if def.windup <= caveSpiderRow.windup {
			t.Errorf("%s winds up in %v, no longer than the spider's quick bite of %v", kind, def.windup, caveSpiderRow.windup)
		}
		if def.damage <= caveSpiderRow.damage {
			t.Errorf("%s hits for %d, no more than the spider's %d", kind, def.damage, caveSpiderRow.damage)
		}
	}
	// It fits the one-block burrows a wave pours out of.
	if caveSpiderRow.body.width >= 1 {
		t.Errorf("a cave spider is %v wide and cannot pass a one-block burrow", caveSpiderRow.body.width)
	}
}

// The scorpion is the opposite: the slowest thing in the game, armoured, and its heavy
// sting is the readable one.
func TestAScorpionIsSlowArmouredAndItsStingIsTheReadableBlow(t *testing.T) {
	t.Parallel()

	for kind, def := range mobRegistry {
		if kind != vnet.MobKindScorpion && def.speed <= scorpionRow.speed {
			t.Errorf("%s at %v is no faster than the scorpion's %v", kind, def.speed, scorpionRow.speed)
		}
	}
	if scorpionRow.armour == 0 {
		t.Error("the scorpion has no shell")
	}
	sting, swipe := scorpionRow, scorpionRow.swipe
	if sting.windup <= swipe.windup || sting.damage <= swipe.damage {
		t.Errorf("the sting (%v, %d) is not the slower, heavier blow against the swipe (%v, %d)",
			sting.windup, sting.damage, swipe.windup, swipe.damage)
	}
	if sting.windup < 900*time.Millisecond || sting.windup > 1500*time.Millisecond {
		t.Errorf("the sting winds up for %v, outside the design's 0.9–1.5 s signal band", sting.windup)
	}
	if scorpionRow.emergeRange != 5 {
		t.Errorf("a scorpion rises for a player within %v blocks, want 5", scorpionRow.emergeRange)
	}
}

// Neither hour of the open world ever offers a dungeon-only species.
func TestTheOpenWorldNeverOffersADungeonOnlySpecies(t *testing.T) {
	t.Parallel()

	for _, night := range []bool{false, true} {
		for _, kind := range spawnableSpecies(night) {
			if mobRegistry[kind].dungeonOnly {
				t.Errorf("the director offers dungeon-only %s (night=%v)", kind, night)
			}
		}
	}
	for _, kind := range []vnet.MobKind{vnet.MobKindCaveSpider, vnet.MobKindScorpion} {
		if !mobRegistry[kind].dungeonOnly {
			t.Errorf("%s is not dungeon-only", kind)
		}
	}
}

// And the director, run for real through a night and a day, never produces one.
func TestTheOpenWorldSpawnPassNeverProducesADescentSpecies(t *testing.T) {
	t.Parallel()

	for _, night := range []bool{false, true} {
		h := newVitalsHarness(t, DefaultTickRate, spawnGround{groundTop: 63})
		if night {
			h.keepNight()
		} else {
			h.keepDay()
		}
		h.join(1, [3]float32{0.5, 64, 0.5})
		seen := h.speciesOverPasses(200)
		for _, kind := range []vnet.MobKind{vnet.MobKindCaveSpider, vnet.MobKindScorpion} {
			if seen[kind] != 0 {
				t.Errorf("two hundred passes (night=%v) produced %d %s", night, seen[kind], kind)
			}
		}
	}
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

// A rusty swing lands on a scorpion for sixty percent of its worth.
func TestAScorpionsShellTurnsAsideItsShareOfABlow(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	// Yaw 0 looks along -Z, so this scorpion is directly ahead and inside reach.
	id := h.spawnMobAt(vnet.MobKindScorpion, [3]float32{0.5, 64, -1.5})

	if err := h.swing(player, mainHandSlot, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	want := scorpionRow.maxHealth - RustySwordDamage*(ArmourScale-scorpionRow.armour)/ArmourScale
	if got := h.mobHealth(id); got != want {
		t.Errorf("the scorpion has %d health after a rusty swing, want %d", got, want)
	}
}

// The shell never turns a blow into nothing, and an unarmoured species takes it whole.
func TestTheShellNeverErasesABlow(t *testing.T) {
	t.Parallel()

	scorpion := &mob{kind: vnet.MobKindScorpion}
	if got := scorpion.armoured(1); got != 1 {
		t.Errorf("a one-point blow lands on a scorpion for %d", got)
	}
	if got := scorpion.armoured(OrbDamage); got == 0 || got >= OrbDamage {
		t.Errorf("an orb lands on a scorpion for %d of %d", got, OrbDamage)
	}
	draugr := &mob{kind: vnet.MobKindDraugr}
	if got := draugr.armoured(RustySwordDamage); got != RustySwordDamage {
		t.Errorf("an unarmoured draugr took %d of a %d blow", got, RustySwordDamage)
	}
}

// ---------------------------------------------------------------------------
// Two attacks, one rhythm
// ---------------------------------------------------------------------------

// The sting opens, then the swipe, each behind its own telegraph and each for its own
// damage — and the rhythm is the server's, counted in ticks.
func TestAScorpionStingsThenSwipes(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnMobAt(vnet.MobKindScorpion, [3]float32{0.5, 64, 2.0})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	timings := h.sim.mobTimings[vnet.MobKindScorpion]

	// telegraph steps until the next windup starts and counts the ticks until it lands.
	telegraph := func(before uint16) (windup int, after uint16) {
		t.Helper()
		for range 200 {
			if h.mob(id).action == vnet.MobActionWindup {
				break
			}
			h.step()
		}
		if h.mob(id).action != vnet.MobActionWindup {
			t.Fatal("the scorpion never committed to an attack")
		}
		for h.vitals(player).Health == before {
			if windup > 200 {
				t.Fatal("the telegraph never landed")
			}
			h.step()
			windup++
		}
		return windup, h.vitals(player).Health
	}

	stingTicks, afterSting := telegraph(PlayerMaxHealth)
	if stingTicks != int(timings.windup) || afterSting != PlayerMaxHealth-scorpionRow.damage {
		t.Errorf("the opening blow landed after %d ticks for %d, want the sting: %d ticks for %d",
			stingTicks, PlayerMaxHealth-afterSting, timings.windup, scorpionRow.damage)
	}
	if !h.mob(id).swiping {
		t.Error("the rhythm did not turn to the swipe after the sting")
	}

	swipeTicks, afterSwipe := telegraph(afterSting)
	if swipeTicks != int(timings.swipeWindup) || afterSting-afterSwipe != scorpionRow.swipe.damage {
		t.Errorf("the second blow landed after %d ticks for %d, want the swipe: %d ticks for %d",
			swipeTicks, afterSting-afterSwipe, timings.swipeWindup, scorpionRow.swipe.damage)
	}
	if h.mob(id).swiping {
		t.Error("the rhythm did not turn back to the sting after the swipe")
	}
}

// Every telegraph and rise survives every tick rate, as the main attack's does.
func TestTheDescentTimingsSurviveEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{1, 2, 5, 20, 60, 255} {
		got := mobTimingsFor(rate)[vnet.MobKindScorpion]
		if got.swipeWindup == 0 || got.swipeRecovery == 0 || got.emergence == 0 {
			t.Errorf("at %d Hz the scorpion's swipe or rise rounds to nothing: %+v", rate, got)
		}
		if spider := mobTimingsFor(rate)[vnet.MobKindCaveSpider]; spider.swipeWindup != 0 || spider.emergence != 0 {
			t.Errorf("at %d Hz the spider has timings for a swipe or rise it does not have: %+v", rate, spider)
		}
	}
}

// ---------------------------------------------------------------------------
// Burial
// ---------------------------------------------------------------------------

// wideZone is a leash that holds everywhere a test in this file walks.
var wideZone = box{min: [3]float64{-64, 0, -64}, max: [3]float64{64, 128, 64}}

func (h *vitalsHarness) placeMinor(kind vnet.MobKind, pos [3]float64, zone box, buried bool) uint64 {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	id, made := h.sim.placeMinorMobLocked(kind, pos, zone, buried)
	if !made {
		h.t.Fatalf("the simulation refused to place a %s", kind)
	}
	return id
}

// A player's standing z at which the gap between their body and a risen scorpion at
// z = 0.5 is exactly gap blocks, both on the same x and the same floor.
func scorpionGapZ(gap float64) float64 {
	return 0.5 + scorpionRow.body.width/2 + PlayerWidth/2 + gap
}

// A buried scorpion lies a block under the sand, idle, and rises for the first player
// inside five blocks — not one step sooner.
func TestABuriedScorpionRisesOnlyInsideFiveBlocks(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	risen := [3]float64{0.5, 64, 0.5}
	id := h.placeMinor(vnet.MobKindScorpion, risen, wideZone, true)
	player, _ := h.join(1, [3]float32{0.5, 64, float32(scorpionGapZ(scorpionRow.emergeRange + 0.2))})

	h.advance(10)
	m, _ := h.mobState(id)
	if !m.buried || m.action != vnet.MobActionIdle || m.pos[1] != risen[1]-burialDepth {
		t.Fatalf("a scorpion with nobody inside five blocks is buried=%v %s at y=%v, want buried Idle at %v",
			m.buried, m.action, m.pos[1], risen[1]-burialDepth)
	}

	h.standAt(player, [3]float64{0.5, 64, scorpionGapZ(scorpionRow.emergeRange - 0.2)})
	h.step()
	m, _ = h.mobState(id)
	if m.buried || m.pos != risen {
		t.Fatalf("a player inside five blocks left it buried=%v at %v, want risen at %v", m.buried, m.pos, risen)
	}
	if m.action != vnet.MobActionRecovery || m.actionTicks != h.sim.mobTimings[vnet.MobKindScorpion].emergence {
		t.Errorf("a risen scorpion is %s for %d ticks, want Recovery for the %d-tick emergence",
			m.action, m.actionTicks, h.sim.mobTimings[vnet.MobKindScorpion].emergence)
	}

	// And then it fights normally: the sting's windup follows the rise.
	for range 200 {
		if h.mob(id).action == vnet.MobActionWindup {
			return
		}
		h.step()
	}
	t.Error("a risen scorpion never committed to an attack")
}

// A dead player brings nothing up.
func TestADeadPlayerDoesNotWakeABuriedScorpion(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.placeMinor(vnet.MobKindScorpion, [3]float64{0.5, 64, 0.5}, wideZone, true)
	player, _ := h.join(1, [3]float32{0.5, 64, float32(scorpionGapZ(1))})
	h.sim.mu.Lock()
	player.damageLocked(player.health)
	h.sim.mu.Unlock()

	h.advance(5)
	if m, _ := h.mobState(id); !m.buried {
		t.Error("a dead player brought a buried scorpion up")
	}
}

// Buried is untargetable: a swing standing over it, an arrow's path through it and damage
// credited straight at it all land on nothing, and nobody taps it.
func TestABuriedScorpionIsNobodysTarget(t *testing.T) {
	t.Parallel()

	// The ground is a block lower than the scorpion's risen floor, so the buried body sits
	// in open air: nothing but the buried state stands between it and a blade or an arrow.
	// Asked under the lock before any tick, so it cannot rise in between.
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 62})
	id := h.placeMinor(vnet.MobKindScorpion, [3]float64{0.5, 64, -1.5}, wideZone, true)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	if target := h.sim.swingTargetLocked(player); target != nil {
		t.Errorf("a swing found creature %d with only a buried scorpion ahead", target.entityID)
	}
	h.sim.creditMobDamageLocked(player, m, IronSwordDamage)
	buried := m.species().body.boxAt(m.pos)
	centre := boxCentre(buried)
	from := [3]float64{centre[0], centre[1], centre[2] + 3}
	to := [3]float64{centre[0], centre[1], centre[2] - 3}
	hit := h.sim.firstProjectileTargetLocked(&projectile{kind: vnet.ProjectileKindArrow}, nil, []*mob{m}, from, to)
	health, tap := m.health, m.firstHit
	h.sim.mu.Unlock()

	if health != scorpionRow.maxHealth || tap != nil {
		t.Errorf("damage reached a buried scorpion: health %d of %d, tapped=%v", health, scorpionRow.maxHealth, tap != nil)
	}
	if hit != nil {
		t.Error("an arrow through a buried scorpion's body found it")
	}
}

// A buried scorpion travels in the snapshot — as Idle, with its body inside the sand —
// so a client can draw the disturbance.
func TestABuriedScorpionIsSentIdleInsideTheSand(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.placeMinor(vnet.MobKindScorpion, [3]float64{0.5, 64, 0.5}, wideZone, true)
	_, out := h.join(1, [3]float32{0.5, 64, 12.5})
	h.step()

	for _, state := range newestSnapshotMobs(t, out) {
		if state.EntityID != id {
			continue
		}
		if state.Kind != vnet.MobKindScorpion || state.Action != vnet.MobActionIdle {
			t.Errorf("a buried scorpion is sent as %s %s, want Scorpion Idle", state.Kind, state.Action)
		}
		// The whole body is inside the solid cell below the surface at y = 64.
		if top := float64(state.Pos[1]) + scorpionRow.body.height; float64(state.Pos[1]) < 63 || top > 64 {
			t.Errorf("a buried scorpion is sent spanning y %v..%v, want inside the sand cell 63..64", state.Pos[1], top)
		}
		return
	}
	t.Error("a buried scorpion is missing from the snapshot")
}

// Something built where it would rise keeps it down: forced up, it would be inside a block.
func TestABuriedScorpionUnderABlockStaysDown(t *testing.T) {
	t.Parallel()

	// A shelf from x = 0 at y = 64 covers the spot the scorpion would rise into.
	h := newVitalsHarness(t, DefaultTickRate, stepTerrain{groundTop: 63, shelfFromX: -1, shelfTop: 64})
	id := h.placeMinor(vnet.MobKindScorpion, [3]float64{0.5, 64, 0.5}, wideZone, true)
	h.join(1, [3]float32{0.5, 65, float32(scorpionGapZ(1))})

	h.advance(5)
	if m, _ := h.mobState(id); !m.buried {
		t.Error("a buried scorpion rose into a solid block")
	}
}

// The placement refuses what cannot be: burial for a species that never lies buried, a
// boss, and a creature placed outside its own zone.
func TestMinorPlacementRefusesWhatCannotBe(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	for _, c := range []struct {
		name   string
		kind   vnet.MobKind
		pos    [3]float64
		buried bool
	}{
		{"a buried spider", vnet.MobKindCaveSpider, [3]float64{0.5, 64, 0.5}, true},
		{"a boss", vnet.MobKindDraugrKing, [3]float64{0.5, 64, 0.5}, false},
		{"outside its zone", vnet.MobKindCaveSpider, [3]float64{100.5, 64, 0.5}, false},
		{"an unknown kind", vnet.MobKindUnknown, [3]float64{0.5, 64, 0.5}, false},
	} {
		if id, made := h.sim.placeMinorMobLocked(c.kind, c.pos, wideZone, c.buried); made || id != 0 {
			t.Errorf("%s was placed as creature %d", c.name, id)
		}
	}
	if len(h.sim.mobs) != 0 {
		t.Errorf("refused placements left %d creatures", len(h.sim.mobs))
	}
}

// ---------------------------------------------------------------------------
// The leash
// ---------------------------------------------------------------------------

// caveZone is a zone that ends at z = 6: the checkpoint, or the door, is past it.
var caveZone = box{min: [3]float64{-6, 60, -20}, max: [3]float64{6, 70, 6}}

// A spider hunts a player inside its zone and does not follow them out of it: it stops at
// the boundary, forgets them, and stays inside for as long as they stay out.
func TestALeashedSpiderDoesNotFollowPastItsZone(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.placeMinor(vnet.MobKindCaveSpider, [3]float64{0.5, 64, -8.5}, caveZone, false)
	player, _ := h.join(1, [3]float32{0.5, 64, 2.5})

	h.step()
	if m, _ := h.mobState(id); m.target != player.entityID || m.action != vnet.MobActionChase {
		t.Fatalf("a spider with a player inside its zone is %s at target %d, want chasing %d",
			m.action, m.target, player.entityID)
	}

	// The player steps out past the boundary, still well inside the spider's awareness.
	h.standAt(player, [3]float64{0.5, 64, 10.5})
	for tick := range 3 * DefaultTickRate {
		h.step()
		m, _ := h.mobState(id)
		if m.pos[2] > caveZone.max[2] {
			t.Fatalf("tick %d: the spider stands at z=%v, past its zone's edge at %v", tick, m.pos[2], caveZone.max[2])
		}
	}
	m, _ := h.mobState(id)
	if m.target != 0 || m.action != vnet.MobActionIdle {
		t.Errorf("a spider whose player left its zone is %s at target %d, want Idle with no target", m.action, m.target)
	}
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("a player outside the zone lost %d health", PlayerMaxHealth-got)
	}
}

// A player who never entered the zone is never prey, even inside the spider's awareness
// and even after hurting it.
func TestALeashedSpiderIgnoresAPlayerOutsideItsZone(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.placeMinor(vnet.MobKindCaveSpider, [3]float64{0.5, 64, 4.5}, caveZone, false)
	player, _ := h.join(1, [3]float32{0.5, 64, 9.5})

	h.sim.mu.Lock()
	h.sim.mobs[id].addThreatLocked(player.entityID, 50)
	h.sim.mu.Unlock()
	h.advance(DefaultTickRate)

	if m, _ := h.mobState(id); m.target != 0 || m.pos[2] > caveZone.max[2] {
		t.Errorf("a spider hunts target %d from z=%v with its only player outside the zone", m.target, m.pos[2])
	}
}

// The clamp trims a step at the boundary, leaves a step inward alone, and never pushes a
// creature already outside.
func TestTheLeashClampTrimsOnlyOutwardSteps(t *testing.T) {
	t.Parallel()

	l := &mobLeash{zone: caveZone}
	for _, c := range []struct {
		name      string
		pos       [3]float64
		delta     [3]float64
		wantDelta [3]float64
	}{
		{"outward across the edge", [3]float64{0, 64, 5.9}, [3]float64{0, 0, 0.3}, [3]float64{0, 0, 6 - 5.9}},
		{"inward", [3]float64{0, 64, 5.9}, [3]float64{0, 0, -0.3}, [3]float64{0, 0, -0.3}},
		{"already outside, further out", [3]float64{0, 64, 7}, [3]float64{0, 0, 0.3}, [3]float64{0, 0, 0}},
		{"already outside, back in", [3]float64{0, 64, 7}, [3]float64{0, 0, -0.3}, [3]float64{0, 0, -0.3}},
		{"vertical is the terrain's", [3]float64{0, 64, 0}, [3]float64{0, -20, 0}, [3]float64{0, -20, 0}},
	} {
		delta, vel := c.delta, [3]float64{1, 1, 1}
		l.clamp(c.pos, &delta, &vel)
		if math.Abs(delta[0]-c.wantDelta[0])+math.Abs(delta[1]-c.wantDelta[1])+math.Abs(delta[2]-c.wantDelta[2]) > 1e-9 {
			t.Errorf("%s: delta %v, want %v", c.name, delta, c.wantDelta)
		}
	}
	var none *mobLeash
	if !none.holds([3]float64{1e9, -1e9, 1e9}) {
		t.Error("an unleashed creature is outside a zone it does not have")
	}
}
