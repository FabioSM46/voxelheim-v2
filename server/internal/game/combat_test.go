package game

import (
	"math"
	"sync"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The swing is a request and the verdict is the server's. Every test here asks what the
// *server* decided, from state no client supplied.

// aimAt points a player's retained look direction at a yaw and pitch, the way an accepted
// PlayerInput would.
func (h *vitalsHarness) aimAt(p *Player, yaw, pitch float64) {
	h.t.Helper()
	if err := p.Submit(protocol.PlayerInput{
		ClientTick: uint32(h.tick) + 1, Yaw: float32(yaw), Pitch: float32(pitch),
	}); err != nil {
		h.t.Fatalf("Submit: %v", err)
	}
}

// swing asks for one attack from a slot, and returns whatever the simulation refused it
// with.
func (h *vitalsHarness) swing(p *Player, slot uint8, tick uint32) error {
	return p.Attack(protocol.AttackRequest{Slot: slot, ClientTick: tick})
}

// mobHealth is what a mob has left, or zero once it has left the world.
func (h *vitalsHarness) mobHealth(id uint64) uint16 {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if m, ok := h.sim.mobs[id]; ok {
		return m.health
	}
	return 0
}

// armedHarness is a player holding the starter blade with a draugr placed relative to
// them, on flat ground, with the player already looking along -Z.
func armedHarness(t *testing.T, rate uint8, draugrAt [3]float32) (*vitalsHarness, *Player, uint64) {
	t.Helper()

	h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	id := h.spawnDraugrAt(draugrAt)
	return h, player, id
}

// ---------------------------------------------------------------------------
// A swing that lands
// ---------------------------------------------------------------------------

func TestASwingInFrontLandsForItsFullDamage(t *testing.T) {
	t.Parallel()

	// Yaw 0 looks along -Z, so this draugr is directly ahead and inside reach.
	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	if got := h.mobHealth(id); got != draugrRow.maxHealth-RustySwordDamage {
		t.Errorf("the draugr has %d health, want %d", got, draugrRow.maxHealth-RustySwordDamage)
	}
}

// Three of them, which is what makes the blade's damage the number that matters rather
// than a decoration on it.
func TestThreeSwingsKillADraugr(t *testing.T) {
	t.Parallel()

	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

	for blow := range 3 {
		if err := h.swing(player, 0, uint32(blow+1)); err != nil {
			t.Fatalf("swing %d was refused: %v", blow+1, err)
		}
		h.step()
		// Past the cooldown before the next one, which is the server's cadence and not
		// the client's.
		h.advance(int(h.sim.attackCooldown))
	}

	h.sim.mu.Lock()
	_, alive := h.sim.mobs[id]
	h.sim.mu.Unlock()

	if alive {
		t.Errorf("the draugr survived three blows with %d health", h.mobHealth(id))
	}
}

// ---------------------------------------------------------------------------
// Where a swing reaches
// ---------------------------------------------------------------------------

func TestASwingReachesExactlyAsFarAsItClaims(t *testing.T) {
	t.Parallel()

	// Body to body, worked out rather than guessed: the player's box ends at z = 0.2 and
	// a draugr at z has its near face at z + 0.3, so the gap is -0.1 - z. That reaches
	// SwordReach exactly at z = -2.6, which is what these two sit either side of.
	for name, tc := range map[string]struct {
		z   float32
		hit bool
	}{
		"just inside the reach":  {z: -2.55, hit: true},
		"just outside the reach": {z: -2.65, hit: false},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, tc.z})
			if err := h.swing(player, 0, 1); err != nil {
				t.Fatalf("the swing was refused: %v", err)
			}
			h.step()

			hurt := h.mobHealth(id) < draugrRow.maxHealth
			if hurt != tc.hit {
				t.Errorf("the draugr at z=%v was hurt=%v, want %v", tc.z, hurt, tc.hit)
			}
		})
	}
}

// The arc is an angle, not a box: turning away misses something still within reach.
func TestASwingOnlyReachesInsideItsArc(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		degrees float64
		hit     bool
	}{
		"straight ahead":  {degrees: 0, hit: true},
		"inside the arc":  {degrees: 40, hit: true},
		"outside the arc": {degrees: 50, hit: false},
		"directly behind": {degrees: 180, hit: false},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			// Placed on a circle about the player, at a radius that is inside the reach
			// whichever way it is turned.
			const radius = 1.3
			radians := tc.degrees * math.Pi / 180
			at := [3]float32{
				float32(0.5 + radius*math.Sin(radians)),
				64,
				float32(0.5 - radius*math.Cos(radians)),
			}

			h, player, id := armedHarness(t, DefaultTickRate, at)
			if err := h.swing(player, 0, 1); err != nil {
				t.Fatalf("the swing was refused: %v", err)
			}
			h.step()

			hurt := h.mobHealth(id) < draugrRow.maxHealth
			if hurt != tc.hit {
				t.Errorf("a draugr %v degrees off the aim was hurt=%v, want %v", tc.degrees, hurt, tc.hit)
			}
		})
	}
}

// Pitch is carried and it decides swings, which is the whole reason the simulation
// started keeping it.
func TestASwingObeysThePlayersPitch(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		pitch float64
		hit   bool
	}{
		"looking down at it":    {pitch: -math.Pi / 2, hit: true},
		"looking level past it": {pitch: 0, hit: false},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			// Two blocks below the player's feet, so the only way to it is downwards.
			h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 62, 0.5})
			h.aimAt(player, 0, tc.pitch)
			if err := h.swing(player, 0, 1); err != nil {
				t.Fatalf("the swing was refused: %v", err)
			}
			h.step()

			hurt := h.mobHealth(id) < draugrRow.maxHealth
			if hurt != tc.hit {
				t.Errorf("a pitch of %v hurt=%v, want %v", tc.pitch, hurt, tc.hit)
			}
		})
	}
}

// A forged pitch is an angle a body could look along, whatever finite number arrives.
func TestAForgedPitchBecomesADirection(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	var tick uint32
	for _, pitch := range []float32{100, -100, 3.14159, -3.14159, 1e30, -1e30} {
		tick++
		if err := player.Submit(protocol.PlayerInput{ClientTick: tick, Pitch: pitch}); err != nil {
			t.Fatalf("a finite pitch of %v was refused: %v", pitch, err)
		}
		h.sim.mu.Lock()
		kept := player.current.pitch
		h.sim.mu.Unlock()

		if kept < -math.Pi/2 || kept > math.Pi/2 {
			t.Errorf("a pitch of %v was kept as %v, outside straight up and straight down", pitch, kept)
		}
	}
}

// A non-finite pitch never becomes a look direction, because it never becomes an intent.
//
// The guard is `Submit`'s, and it predates this issue —
// TestNonFiniteInputIsRefusedAndLeavesThePositionFiniteAndUnchanged covers the refusal
// itself for every axis including this one. What that test could not assert is what this
// issue made new: pitch is now *load-bearing*, so the question is no longer only whether
// the position survives but whether the aim does. A NaN reaching lookDirection would make
// every dot product false and every swing miss for the rest of the session, silently.
func TestANonFinitePitchNeverReachesTheAim(t *testing.T) {
	t.Parallel()

	for name, pitch := range map[string]float32{
		"NaN":  float32(math.NaN()),
		"+Inf": float32(math.Inf(1)),
		"-Inf": float32(math.Inf(-1)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

			// Aim at it properly first, so a refused frame has something to fail to undo.
			h.aimAt(player, 0, 0)
			if err := player.Submit(protocol.PlayerInput{ClientTick: 500, Pitch: pitch}); err == nil {
				t.Fatal("the simulation accepted a non-finite pitch")
			}

			h.sim.mu.Lock()
			kept := player.current.pitch
			h.sim.mu.Unlock()
			if math.IsNaN(kept) || math.IsInf(kept, 0) {
				t.Fatalf("the retained pitch is %v after a refused frame", kept)
			}

			// And the aim still works, which is the half a finiteness check alone would
			// not have shown.
			if err := h.swing(player, 0, 501); err != nil {
				t.Fatalf("the swing was refused: %v", err)
			}
			h.step()
			if got := h.mobHealth(id); got != draugrRow.maxHealth-RustySwordDamage {
				t.Errorf("the draugr has %d health after the swing, want %d", got, draugrRow.maxHealth-RustySwordDamage)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// What the slot has to hold
// ---------------------------------------------------------------------------

func TestOnlyAWorkingBladeSwings(t *testing.T) {
	t.Parallel()

	for name, prepare := range map[string]func(*Player) uint8{
		"an empty slot": func(*Player) uint8 { return 5 },
		"a slot of stone": func(p *Player) uint8 {
			p.inventory.mu.Lock()
			defer p.inventory.mu.Unlock()
			p.inventory.slots[5] = stackOf(ItemStone, 10)
			return 5
		},
		"a blade worn through": func(p *Player) uint8 {
			p.inventory.mu.Lock()
			defer p.inventory.mu.Unlock()
			p.inventory.slots[0].durability = 0
			return 0
		},
		"a slot outside the inventory": func(*Player) uint8 { return protocol.InventorySlots },
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})
			slot := prepare(player)

			// A refusal at admission and a refusal on the tick are both silence to the
			// client; either is acceptable, and neither may hurt anything.
			_ = h.swing(player, slot, 1)
			h.step()

			if got := h.mobHealth(id); got != draugrRow.maxHealth {
				t.Errorf("%s took %d health off the draugr", name, draugrRow.maxHealth-got)
			}
		})
	}
}

// A hit costs the blade nothing in this iteration. Only death wears equipment.
func TestALandedSwingCostsTheBladeNothing(t *testing.T) {
	t.Parallel()

	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})
	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	if h.mobHealth(id) == draugrRow.maxHealth {
		t.Fatal("the swing did not land, so this proves nothing about durability")
	}
	if got := player.InventoryState().Stacks[0]; got != starterSword() {
		t.Errorf("the blade is %+v after a hit, want the untouched %+v", got, starterSword())
	}
}

// ---------------------------------------------------------------------------
// Cadence
// ---------------------------------------------------------------------------

// The cooldown is the server's answer to how often a blade swings, at any rate Step is
// called at and whatever the client asks for.
func TestTheCooldownBoundsTheSwingRateAtEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{5, 20, 60} {
		t.Run("tick rate "+string(rune('0'+rate%10)), func(t *testing.T) {
			h, player, id := armedHarness(t, rate, [3]float32{0.5, 64, -1.5})

			// A client clicking on every tick for a second.
			var tick uint32
			for range int(rate) {
				tick++
				_ = h.swing(player, 0, tick)
				h.step()
			}

			// A second of contact is bounded by the cooldown, not by the tick count: at
			// 600ms a second holds one swing and the beginning of another.
			landed := int(draugrRow.maxHealth-h.mobHealth(id)) / int(RustySwordDamage)
			if landed < 1 {
				t.Errorf("%d Hz: nothing landed in a second of clicking", rate)
			}
			if landed > 2 {
				t.Errorf("%d Hz: %d blows landed in a second; the cooldown bounds it", rate, landed)
			}
		})
	}
}

// A miss pays the same cooldown a hit does, so asking whether anything is there costs
// what connecting with it costs.
func TestAMissPaysTheCooldown(t *testing.T) {
	t.Parallel()

	// Nothing within reach: the draugr is placed far away.
	h, player, _ := armedHarness(t, DefaultTickRate, [3]float32{40.5, 64, 0.5})

	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the first swing was refused: %v", err)
	}
	h.step()

	h.sim.mu.Lock()
	cooldown := player.attackCooldown
	h.sim.mu.Unlock()
	if cooldown == 0 {
		t.Fatal("a miss paid no cooldown")
	}

	if err := h.swing(player, 0, 2); err == nil {
		t.Error("a second swing was accepted while the blade was recovering")
	}
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

func TestAdmissionRefusesWhatItShould(t *testing.T) {
	t.Parallel()

	h, player, _ := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

	if err := h.swing(player, 0, 5); err != nil {
		t.Fatalf("the first swing was refused: %v", err)
	}
	// A replayed tick.
	if err := h.swing(player, 0, 5); err == nil {
		t.Error("a replayed attack tick was accepted")
	}
	// An older one.
	if err := h.swing(player, 0, 4); err == nil {
		t.Error("a stale attack tick was accepted")
	}
	// A second click before the tick has judged the first.
	if err := h.swing(player, 0, 6); err == nil {
		t.Error("a second swing queued behind the first")
	}

	// And the dead ask for nothing.
	h.step()
	h.advance(int(h.sim.attackCooldown))
	h.hurt(player, PlayerMaxHealth)
	if err := h.swing(player, 0, 100); err == nil {
		t.Error("a dead player's swing was accepted")
	}
}

// A swing accepted before the blow that killed them does not land afterwards.
func TestDeathDropsAPendingSwing(t *testing.T) {
	t.Parallel()

	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})
	// Night, because this is the one combat test whose draugr ends the tick with
	// nothing to hunt: the player it was hunting is the corpse. In daylight the
	// director would take it away, and "the swing landed nothing" and "there was
	// nothing left to swing at" would read identically.
	h.keepNight()
	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.hurt(player, PlayerMaxHealth)
	h.step()

	if got := h.mobHealth(id); got != draugrRow.maxHealth {
		t.Errorf("a corpse's swing took %d health off the draugr", draugrRow.maxHealth-got)
	}
}

// ---------------------------------------------------------------------------
// Choosing what to hit
// ---------------------------------------------------------------------------

func TestASwingTakesTheNearestAndBreaksTiesByIdentity(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	far := h.spawnDraugrAt([3]float32{0.5, 64, -2.0})
	near := h.spawnDraugrAt([3]float32{0.5, 64, -1.2})

	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	if h.mobHealth(near) == draugrRow.maxHealth {
		t.Error("the nearer draugr was untouched")
	}
	if h.mobHealth(far) != draugrRow.maxHealth {
		t.Error("the swing reached past the nearer draugr")
	}
}

// Players are not targets, whatever is standing in the arc.
func TestASwingNeverTouchesAnotherPlayer(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	attacker, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	victim, _ := h.join(2, [3]float32{0.5, 64, -1.2})

	if err := h.swing(attacker, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	if got := h.vitals(victim).Health; got != PlayerMaxHealth {
		t.Errorf("the swing cost another player %d health", PlayerMaxHealth-got)
	}
}

// The tick order, asserted where it is observable: a draugr killed by a swing does not
// land the blow it was winding up in the same tick.
func TestADraugrKilledBySwingLandsNoBlowThatTick(t *testing.T) {
	t.Parallel()

	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

	// Wound it to within one blow, and let it commit to an attack that is one tick from
	// landing.
	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	m.health = RustySwordDamage
	m.action = vnet.MobActionWindup
	m.actionTicks = 1
	m.target = player.entityID
	h.sim.mu.Unlock()

	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}
	h.step()

	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("a draugr killed this tick still cost %d health", PlayerMaxHealth-got)
	}
	if h.mobCount() != 0 {
		t.Errorf("the draugr survived a blow that should have killed it: %d health", h.mobHealth(id))
	}
}

// ---------------------------------------------------------------------------
// Contention
// ---------------------------------------------------------------------------

// The tick never waits for a session goroutine, and a swing the player made does not
// stop having been made because one of their own messages was in flight.
func TestAContendedInventoryDefersASwingWithoutLosingIt(t *testing.T) {
	t.Parallel()

	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})
	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused: %v", err)
	}

	player.inventory.mu.Lock()
	h.advance(5)
	if got := h.mobHealth(id); got != draugrRow.maxHealth {
		t.Fatalf("a swing landed while the inventory was unreadable: %d health", got)
	}
	h.sim.mu.Lock()
	stillPending := player.pendingSwing != nil
	h.sim.mu.Unlock()
	if !stillPending {
		t.Fatal("the deferred swing was thrown away")
	}
	player.inventory.mu.Unlock()

	h.step()
	if got := h.mobHealth(id); got != draugrRow.maxHealth-RustySwordDamage {
		t.Errorf("the deferred swing landed for %d, want %d", draugrRow.maxHealth-got, RustySwordDamage)
	}

	// And exactly once, however many ticks follow.
	h.advance(10)
	if got := h.mobHealth(id); got != draugrRow.maxHealth-RustySwordDamage {
		t.Errorf("the deferred swing landed twice: %d health left", got)
	}
}

// Attacks, inventory movement, mob stepping and a disconnect, interleaved. This is the
// shape -race is here to judge.
func TestSwingsUnderConcurrentSessionTraffic(t *testing.T) {
	t.Parallel()

	h, player, _ := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})
	other, _ := h.join(2, [3]float32{20.5, 64, 0.5})

	var wg sync.WaitGroup
	stop := make(chan struct{})
	wg.Add(2)
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
			_ = player.Attack(protocol.AttackRequest{Slot: 0, ClientTick: tick})
			_, _ = player.MoveInventory(protocol.InventoryMoveRequest{From: 0, To: 5, Count: 1})
			_, _ = player.MoveInventory(protocol.InventoryMoveRequest{From: 5, To: 0, Count: 1})
		}
	}()
	go func() {
		defer wg.Done()
		<-stop
		h.sim.Leave(other)
	}()

	h.advance(150)
	close(stop)
	wg.Wait()

	// Nothing to assert beyond survival and the race detector's verdict: what this
	// covers is that the tick and the session goroutines never touch the same state
	// without the lock that owns it.
	if h.sim.Count() == 0 {
		t.Error("every player left the simulation")
	}
}

// ---------------------------------------------------------------------------
// The swing is species-agnostic, and the body it reaches is not
// ---------------------------------------------------------------------------

// The same swing at the same standing distance reaches a vargr and misses a draugr.
//
// **Because reach is measured body to body and the two bodies differ.** A vargr is the
// wider of the two, so its near face is closer at the same distance from the player —
// and a swing that read one hardcoded box would have given both species the same reach
// while appearing to measure each of them. That box was `draugrBody`, built inside
// swingTargetLocked; the bug this test would have caught is the reason it moved into the
// registry with the vargr rather than after it.
//
// The distance is derived from the two rows rather than written down, so this keeps
// meaning the same thing after a rebalance moves either body.
func TestASwingReadsTheBodyItReachesFromTheRegistry(t *testing.T) {
	t.Parallel()

	if vargrRow.body.width <= draugrRow.body.width {
		t.Fatalf("this test needs the two bodies to differ in width: vargr %v, draugr %v",
			vargrRow.body.width, draugrRow.body.width)
	}

	// The furthest a creature of each species can stand and still be inside SwordReach:
	// the player's half-width and the creature's half-width are the two faces the gap is
	// measured between. Halfway between the two is inside one and outside the other.
	reaches := func(shape body) float64 { return SwordReach + PlayerWidth/2 + shape.width/2 }
	distance := (reaches(vargrRow.body) + reaches(draugrRow.body)) / 2

	for kind, wantHit := range map[vnet.MobKind]bool{vnet.MobKindVargr: true, vnet.MobKindDraugr: false} {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		// Yaw 0 looks along -Z, so this is directly ahead of the player.
		id := h.placeSpeciesAt(kind, [3]float64{0.5, 64, 0.5 - distance})

		if err := h.swing(player, 0, 1); err != nil {
			t.Fatalf("the swing at a %s was refused: %v", kind, err)
		}
		h.step()

		hit := h.mobHealth(id) < mobRegistry[kind].maxHealth
		if hit != wantHit {
			t.Errorf("a swing at a %s %v blocks away hit = %v, want %v (body %v wide)",
				kind, distance, hit, wantHit, mobRegistry[kind].body.width)
		}
	}
}

// A vargr goes down in one swing fewer than a draugr.
//
// The trade the species is: it outruns you, and it has less between you and it when you
// stop running. Nothing in the swing knows which of the two it is hitting — the blade is
// worth what the blade is worth, and what differs is the health each row arrives with.
func TestAVargrDiesInOneSwingFewerThanADraugr(t *testing.T) {
	t.Parallel()

	kill := func(t *testing.T, kind vnet.MobKind) int {
		t.Helper()

		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		id := h.placeSpeciesAt(kind, [3]float64{0.5, 64, -1.5})

		for blow := 1; blow <= 10; blow++ {
			if err := h.swing(player, 0, uint32(blow)); err != nil {
				t.Fatalf("swing %d at a %s was refused: %v", blow, kind, err)
			}
			h.step()

			h.sim.mu.Lock()
			_, alive := h.sim.mobs[id]
			h.sim.mu.Unlock()
			if !alive {
				return blow
			}
			// Past the cooldown before the next one, which is the server's cadence and
			// not the client's.
			h.advance(int(h.sim.attackCooldown))
		}
		t.Fatalf("ten blows of the starter blade did not kill a %s", kind)
		return 0
	}

	vargr := kill(t, vnet.MobKindVargr)
	draugr := kill(t, vnet.MobKindDraugr)
	if vargr != draugr-1 {
		t.Errorf("the starter blade needs %d swings for a vargr and %d for a draugr, want exactly one fewer",
			vargr, draugr)
	}
}
