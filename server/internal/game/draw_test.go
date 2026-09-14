package game

import (
	"log/slog"
	"math"
	"math/rand/v2"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The bow is drawn, held and loosed by the server's ticks. Every test here drives the edges a
// client may send and asks what the simulation made of them.

// archer joins one player standing on flat ground with a bow in the main hand and arrows in
// hotbar slot 1, and settles them onto the ground so a mount can fit.
func archer(t *testing.T, arrows uint16) (*vitalsHarness, *Player) {
	t.Helper()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	player.inventory.mu.Lock()
	player.inventory.slots[equipmentMainHand] = stackOf(ItemBow, 1)
	if arrows > 0 {
		player.inventory.slots[1] = stackOf(ItemArrow, arrows)
	}
	player.inventory.mu.Unlock()
	h.fallUntilLanded(player)
	return h, player
}

func (h *vitalsHarness) draw(p *Player, active bool, tick uint32) (vnet.RefusalReason, error) {
	return p.Draw(protocol.DrawRequest{Active: active, ClientTick: tick})
}

// drawState is what the tick holds for a player's draw, read under the lock that owns it.
func drawState(h *vitalsHarness, p *Player) (drawing bool, energy uint32, projectiles int) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return p.draw != nil, p.energy, len(h.sim.projectiles)
}

func arrowsCarried(p *Player) uint16 {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	var count uint16
	for _, stack := range p.inventory.slots[:equipmentFirst] {
		if stack.item == ItemArrow {
			count += stack.count
		}
	}
	return count
}

// A draw begins only with a usable bow in the main hand, an arrow, the energy, a free body and
// a recovered weapon. Every refusal spends nothing; missing arrows, missing energy and a mount
// are answered, everything else is silence.
func TestADrawIsAdmittedOnlyWithABowAnArrowEnergyAndAFreeBody(t *testing.T) {
	t.Parallel()

	cost := uint32(AttackEnergyCost) * energyScale
	for name, tc := range map[string]struct {
		prepare  func(*vitalsHarness, *Player)
		arrows   uint16
		reason   vnet.RefusalReason
		admitted bool
	}{
		"a bow, an arrow and the energy": {arrows: 1, admitted: true},
		"no arrow":                       {reason: vnet.RefusalReasonNoAmmunition},
		"arrows only in a worn slot": {prepare: func(_ *vitalsHarness, p *Player) {
			p.inventory.slots[equipmentHead] = stackOf(ItemArrow, 4)
		}, reason: vnet.RefusalReasonNoAmmunition},
		"a reserve one point short": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.energy = cost - 1
		}, reason: vnet.RefusalReasonNotEnoughEnergy},
		"mounted": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.mounted = vnet.MountKindBrownHorse
		}, reason: vnet.RefusalReasonActionForbiddenWhileMounted},
		"blocking": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.blocking = true
		}},
		"the bow still recovering": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.attackCooldown = 3
		}},
		"a swing already waiting": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.pendingSwing = &pendingSwing{slot: mainHandSlot}
		}},
		"a worn-through bow": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.inventory.slots[equipmentMainHand].durability = 0
		}},
		"a blade in the main hand": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.inventory.slots[equipmentMainHand] = stackOf(ItemIronSword, 1)
		}},
		"a sceptre in the main hand": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.inventory.slots[equipmentMainHand] = stackOf(ItemWoodenSceptre, 1)
		}},
		"a bow in the pack and nothing in the hand": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.inventory.slots[equipmentMainHand] = inventoryStack{}
			p.inventory.slots[5] = stackOf(ItemBow, 1)
		}},
		"a dead player": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.lifeState = vnet.LifeStateDead
		}},
		"a leaving player": {arrows: 1, prepare: func(_ *vitalsHarness, p *Player) {
			p.leaving = true
		}},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player := archer(t, tc.arrows)
			h.sim.mu.Lock()
			player.inventory.mu.Lock()
			if tc.prepare != nil {
				tc.prepare(h, player)
			}
			player.inventory.mu.Unlock()
			energyBefore := player.energy
			h.sim.mu.Unlock()

			reason, err := h.draw(player, true, 1)
			if reason != tc.reason {
				t.Errorf("answered %s (%v), want %s", reason, err, tc.reason)
			}
			drawing, energy, _ := drawState(h, player)
			if !tc.admitted {
				if err == nil {
					t.Error("the draw was admitted, want it refused")
				}
				if drawing || energy != energyBefore {
					t.Errorf("a refused draw left drawing %v and energy %d, want no draw and %d", drawing, energy, energyBefore)
				}
				return
			}
			if err != nil || !drawing {
				t.Fatalf("draw = (%s, %v), drawing %v; want admitted", reason, err, drawing)
			}
			if energy != energyBefore-cost {
				t.Errorf("energy after the press = %d, want %d spent at the start", energy, cost)
			}
		})
	}
}

// A second press without a release is dropped: the draw running is the one being held, and
// neither its charge nor its cost starts again.
func TestARepeatedPressNeitherRestartsNorRepaysTheDraw(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 2)
	if _, err := h.draw(player, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	h.advance(5)
	_, energy, _ := drawState(h, player)
	progress := h.vitals(player).DrawProgress
	if reason, err := h.draw(player, true, 2); err == nil || reason != vnet.RefusalReasonUnknown {
		t.Errorf("a second press = (%s, %v), want silence", reason, err)
	}
	if got := h.vitals(player).DrawProgress; got != progress {
		t.Errorf("progress after the second press = %d, want the unchanged %d", got, progress)
	}
	if _, after, _ := drawState(h, player); after != energy {
		t.Errorf("energy after the second press = %d, want the unchanged %d", after, energy)
	}
}

// The charge is counted in server ticks from the admitted press, projected onto the wire as a
// fraction of 255 that never reads zero while drawing, and it stops at the full draw however
// long the string is held — at no further cost.
func TestTheDrawChargesByServerTicksAndHoldsAtFullForFree(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{DefaultTickRate, 5} {
		h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		player.inventory.mu.Lock()
		player.inventory.slots[equipmentMainHand] = stackOf(ItemBow, 1)
		player.inventory.slots[1] = stackOf(ItemArrow, 1)
		player.inventory.mu.Unlock()

		full := h.sim.fullDrawTicks
		if want := ticksFor(FullDrawDuration, rate); full != want {
			t.Fatalf("rate %d: full draw = %d ticks, want %d", rate, full, want)
		}
		if got := h.vitals(player).DrawProgress; got != 0 {
			t.Errorf("rate %d: progress before any draw = %d, want 0", rate, got)
		}
		if _, err := h.draw(player, true, 1); err != nil {
			t.Fatalf("rate %d: press: %v", rate, err)
		}
		if got := h.vitals(player).DrawProgress; got != 1 {
			t.Errorf("rate %d: progress on the press = %d, want 1", rate, got)
		}
		for held := uint32(1); held <= full+3*uint32(rate); held++ {
			_, before, _ := drawState(h, player)
			h.step()
			want := uint8(255)
			if held < full {
				want = uint8(max(uint64(held)*255/uint64(full), 1))
			}
			if got := h.vitals(player).DrawProgress; got != want {
				t.Fatalf("rate %d: progress after %d held ticks = %d, want %d", rate, held, got, want)
			}
			if _, after, _ := drawState(h, player); after < before {
				t.Fatalf("rate %d: holding the draw spent energy on tick %d: %d -> %d", rate, held, before, after)
			}
		}
	}
}

// A release looses the drawn arrow on the next tick: one arrow and one point of the bow are
// spent, the arrow leaves along the player's aim through the one flight path, the bow's
// cooldown starts, and the wire says the player is no longer drawing.
func TestAReleaseLoosesOneArrowAndStartsTheBowsCooldown(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 2)
	h.aimAt(player, math.Pi/2, math.Pi/6)
	if _, err := h.draw(player, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	// Held to a full draw, so the arrow carries no spread and its direction is the aim exactly.
	h.advance(int(h.sim.fullDrawTicks))
	if reason, err := h.draw(player, false, 2); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("release = (%s, %v), want accepted", reason, err)
	}
	if arrowsCarried(player) != 2 {
		t.Fatal("the release spent an arrow before the tick")
	}

	// Resolved without advancing the projectile, so the launch velocity is read exactly.
	h.sim.mu.Lock()
	player.resolveDrawLocked()
	drawing := player.draw != nil
	cooldown := player.attackCooldown
	var arrow *projectile
	for _, proj := range h.sim.projectiles {
		arrow = proj
	}
	count := len(h.sim.projectiles)
	aim := lookDirection(player.current.yaw, player.current.pitch)
	h.sim.mu.Unlock()

	if drawing {
		t.Error("the draw is still held after its release was loosed")
	}
	if got := h.vitals(player).DrawProgress; got != 0 {
		t.Errorf("progress after the release = %d, want 0", got)
	}
	if cooldown != h.sim.bowCooldownTicks {
		t.Errorf("cooldown after the release = %d, want %d", cooldown, h.sim.bowCooldownTicks)
	}
	if count != 1 || arrow == nil {
		t.Fatalf("projectiles = %d, want one arrow", count)
	}
	if arrow.kind != vnet.ProjectileKindArrow || arrow.owner != player.entityID {
		t.Errorf("projectile kind/owner = %s/%d, want Arrow/%d", arrow.kind, arrow.owner, player.entityID)
	}
	speed := vectorLength(arrow.vel)
	for axis := range 3 {
		if math.Abs(arrow.vel[axis]/speed-aim[axis]) > 1e-9 {
			t.Errorf("direction[%d] = %v, want %v", axis, arrow.vel[axis]/speed, aim[axis])
		}
	}
	if got := arrowsCarried(player); got != 1 {
		t.Errorf("arrows after the release = %d, want 1", got)
	}
	if got := player.InventoryState().Stacks[equipmentMainHand].Durability; got != BowMaxDurability-1 {
		t.Errorf("bow durability = %d, want %d", got, BowMaxDurability-1)
	}

	// The cooldown is what stops the next press, until it has run out.
	if _, err := h.draw(player, true, 3); err == nil {
		t.Error("a press during the bow's cooldown was admitted")
	}
	h.advance(int(h.sim.bowCooldownTicks))
	if _, err := h.draw(player, true, 4); err != nil {
		t.Errorf("a press after the cooldown was refused: %v", err)
	}
}

// A press and a release that both arrive before a tick has elapsed still loose an arrow, at
// no charge at all.
func TestAReleaseBeforeATickHasElapsedStillLooses(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 1)
	if _, err := h.draw(player, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	if _, err := h.draw(player, false, 2); err != nil {
		t.Fatalf("release: %v", err)
	}
	h.sim.mu.Lock()
	held := player.draw.held
	h.sim.mu.Unlock()
	if held != 0 {
		t.Errorf("held = %d before any tick, want 0", held)
	}
	h.step()
	if drawing, _, projectiles := drawState(h, player); drawing || projectiles != 1 {
		t.Errorf("after the tick: drawing %v, projectiles %d; want the arrow loosed", drawing, projectiles)
	}
}

// A release the tick cannot apply because a session goroutine holds the inventory is kept at
// the charge it was made, and looses on the next tick that can.
func TestAReleaseWaitsForABusyInventoryWithoutCharging(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 1)
	if _, err := h.draw(player, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	h.advance(2)
	if _, err := h.draw(player, false, 2); err != nil {
		t.Fatalf("release: %v", err)
	}
	player.inventory.mu.Lock()
	h.step()
	player.inventory.mu.Unlock()

	h.sim.mu.Lock()
	drawing, held, projectiles := player.draw != nil, uint32(0), len(h.sim.projectiles)
	if drawing {
		held = player.draw.held
	}
	h.sim.mu.Unlock()
	if !drawing || held != 2 || projectiles != 0 {
		t.Fatalf("with the inventory busy: drawing %v held %d projectiles %d; want the release kept at 2", drawing, held, projectiles)
	}
	h.step()
	if drawing, _, projectiles := drawState(h, player); drawing || projectiles != 1 {
		t.Errorf("once the inventory was free: drawing %v, projectiles %d; want the arrow loosed", drawing, projectiles)
	}
}

// A release that finds no arrow left fires nothing, and the energy stays spent.
func TestAReleaseWithNoArrowLeftFiresNothing(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 1)
	if _, err := h.draw(player, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	player.inventory.mu.Lock()
	player.inventory.slots[1] = inventoryStack{}
	bowBefore := player.inventory.slots[equipmentMainHand]
	player.inventory.mu.Unlock()
	if _, err := h.draw(player, false, 2); err != nil {
		t.Fatalf("release: %v", err)
	}
	h.step()

	h.sim.mu.Lock()
	drawing, cooldown, projectiles := player.draw != nil, player.attackCooldown, len(h.sim.projectiles)
	h.sim.mu.Unlock()
	if drawing || cooldown != 0 || projectiles != 0 {
		t.Errorf("an arrowless release left drawing %v cooldown %d projectiles %d", drawing, cooldown, projectiles)
	}
	player.inventory.mu.Lock()
	bowAfter := player.inventory.slots[equipmentMainHand]
	player.inventory.mu.Unlock()
	if bowAfter != bowBefore {
		t.Errorf("the bow changed from %+v to %+v", bowBefore, bowAfter)
	}
}

// Every cancellation ends the draw without firing it and without giving back the energy the
// press paid. Each case starts from a reserve that the press empties exactly, and reads the
// reserve before any tick could refill it.
func TestACancelledDrawFiresNothingAndRefundsNothing(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		cancel func(t *testing.T, h *vitalsHarness, p *Player)
		// gone is true when the player is no longer in this simulation afterwards, so there
		// is no release to send and no tick of theirs to observe.
		gone bool
		// ticks is true when the cancellation needs ticks of its own, so energy has refilled
		// by the time it is read and only the draw itself is asserted.
		ticks bool
	}{
		"teleport": {cancel: func(t *testing.T, h *vitalsHarness, p *Player) {
			h.sim.mu.Lock()
			h.sim.devCommands = true
			h.sim.mu.Unlock()
			if _, err := p.Chat("/teleport 0 64 0"); err != nil {
				t.Fatalf("teleport: %v", err)
			}
		}},
		"mounting": {ticks: true, cancel: func(t *testing.T, h *vitalsHarness, p *Player) {
			prepareMount(p, vnet.MountKindGreyHorse, true)
			if reason, err := p.Mount(vnet.MountKindGreyHorse); err != nil || reason != vnet.RefusalReasonUnknown {
				t.Fatalf("Mount: reason %s, error %v", reason, err)
			}
			h.advance(int(h.sim.castTicks))
			if got := mountedKind(p); got != vnet.MountKindGreyHorse {
				t.Fatalf("mounted = %s, want GreyHorse", got)
			}
		}},
		"death": {cancel: func(_ *testing.T, h *vitalsHarness, p *Player) {
			h.hurt(p, PlayerMaxHealth)
		}},
		"beginning to leave": {cancel: func(_ *testing.T, _ *vitalsHarness, p *Player) {
			p.BeginLeaving()
		}},
		"leaving": {gone: true, cancel: func(_ *testing.T, h *vitalsHarness, p *Player) {
			h.sim.Leave(p)
		}},
		"a world transfer": {gone: true, cancel: func(t *testing.T, h *vitalsHarness, p *Player) {
			target, err := NewSim(DefaultTickRate, 8, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{},
				testEntityIDs(), slog.New(slog.DiscardHandler), WithWorldGroup(h.sim.group))
			if err != nil {
				t.Fatalf("NewSim: %v", err)
			}
			if err := h.sim.Transfer(p, target, [3]float32{0.5, 64, 0.5}); err != nil {
				t.Fatalf("Transfer: %v", err)
			}
		}},
		"the bow moved out of the main hand": {cancel: func(t *testing.T, _ *vitalsHarness, p *Player) {
			if _, err := p.MoveInventory(protocol.InventoryMoveRequest{From: mainHandSlot, To: 7, Count: 1}); err != nil {
				t.Fatalf("move: %v", err)
			}
		}},
		// A move never trades one bow for another in an equipment slot (moveLocked refuses a
		// same-item swap there), so the swap that can happen is another weapon taking the
		// bow's place.
		"a blade swapped into the main hand": {cancel: func(t *testing.T, _ *vitalsHarness, p *Player) {
			p.inventory.mu.Lock()
			p.inventory.slots[7] = stackOf(ItemIronSword, 1)
			p.inventory.mu.Unlock()
			if _, err := p.MoveInventory(protocol.InventoryMoveRequest{From: 7, To: mainHandSlot, Count: 1}); err != nil {
				t.Fatalf("move: %v", err)
			}
			p.inventory.mu.Lock()
			held := p.inventory.slots[7].item
			p.inventory.mu.Unlock()
			if held != ItemBow {
				t.Fatalf("slot 7 holds %d after the swap, want the bow", held)
			}
		}},
		"the main hand emptied": {cancel: func(_ *testing.T, h *vitalsHarness, p *Player) {
			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			p.inventory.mu.Lock()
			defer p.inventory.mu.Unlock()
			p.inventory.slots[equipmentMainHand] = inventoryStack{}
			p.refreshWornLocked()
		}},
		"the bow worn out": {cancel: func(_ *testing.T, h *vitalsHarness, p *Player) {
			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			p.inventory.mu.Lock()
			defer p.inventory.mu.Unlock()
			p.inventory.slots[equipmentMainHand].durability = 0
			p.refreshWornLocked()
		}},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player := archer(t, 3)
			h.sim.mu.Lock()
			player.energy = uint32(AttackEnergyCost) * energyScale
			h.sim.mu.Unlock()
			if _, err := h.draw(player, true, 1); err != nil {
				t.Fatalf("press: %v", err)
			}
			h.advance(2)
			h.sim.mu.Lock()
			spent := player.energy
			h.sim.mu.Unlock()

			tc.cancel(t, h, player)

			h.sim.mu.Lock()
			drawing, energy := player.draw != nil, player.energy
			h.sim.mu.Unlock()
			if drawing {
				t.Fatal("the draw survived the cancellation")
			}
			if !tc.ticks && energy != spent {
				t.Errorf("energy after the cancellation = %d, want the spent %d with nothing refunded", energy, spent)
			}
			if tc.gone {
				return
			}
			if got := h.vitals(player).DrawProgress; got != 0 {
				t.Errorf("progress after the cancellation = %d, want 0", got)
			}
			if _, err := h.draw(player, false, 2); err == nil {
				t.Error("a release after the cancellation was admitted")
			}
			h.step()
			if _, _, projectiles := drawState(h, player); projectiles != 0 {
				t.Errorf("a cancelled draw fired %d projectiles", projectiles)
			}
			if got := arrowsCarried(player); got != 3 {
				t.Errorf("arrows after the cancellation = %d, want all 3", got)
			}
		})
	}
}

// Both edges share one ordering guard: a stale release is dropped and the draw it would have
// loosed keeps charging.
func TestAStaleReleaseIsDroppedAndTheDrawKeepsCharging(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 1)
	if _, err := h.draw(player, true, 10); err != nil {
		t.Fatalf("press: %v", err)
	}
	if _, err := h.draw(player, false, 9); err == nil {
		t.Fatal("a release older than the press was admitted")
	}
	h.advance(3)
	if drawing, _, projectiles := drawState(h, player); !drawing || projectiles != 0 {
		t.Errorf("after a stale release: drawing %v, projectiles %d; want the draw still held", drawing, projectiles)
	}
	if got := h.vitals(player).DrawProgress; got <= 1 {
		t.Errorf("progress = %d after three held ticks, want it charging", got)
	}
}

// While the bow is drawn the attack path stays out of it: a swing is dropped before it can
// create pending state, spend energy or start a cooldown.
func TestASwingWhileDrawingIsDropped(t *testing.T) {
	t.Parallel()

	h, player := archer(t, 2)
	if _, err := h.draw(player, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	_, energy, _ := drawState(h, player)
	if reason, err := player.Attack(protocol.AttackRequest{Slot: mainHandSlot, ClientTick: 1}); err == nil || reason != vnet.RefusalReasonUnknown {
		t.Errorf("a swing while drawing = (%s, %v), want silence", reason, err)
	}
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if player.pendingSwing != nil || player.energy != energy || player.draw == nil {
		t.Errorf("a dropped swing left pending %v energy %d (want %d) drawing %v",
			player.pendingSwing != nil, player.energy, energy, player.draw != nil)
	}
}

// ---------------------------------------------------------------------------
// The charged launch
// ---------------------------------------------------------------------------

// looseAfter presses, holds for held ticks, releases, and resolves the release without
// advancing the projectile, so the arrow's launch velocity is read before any gravity step.
func looseAfter(t *testing.T, h *vitalsHarness, p *Player, held int) *projectile {
	t.Helper()

	if _, err := h.draw(p, true, 1); err != nil {
		t.Fatalf("press: %v", err)
	}
	h.advance(held)
	if _, err := h.draw(p, false, 2); err != nil {
		t.Fatalf("release: %v", err)
	}
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.resolveDrawLocked()
	if len(h.sim.projectiles) != 1 {
		t.Fatalf("projectiles after the release = %d, want one arrow", len(h.sim.projectiles))
	}
	for _, proj := range h.sim.projectiles {
		return proj
	}
	return nil
}

func angleBetween(a, b [3]float64) float64 {
	dot := (a[0]*b[0] + a[1]*b[1] + a[2]*b[2]) / (vectorLength(a) * vectorLength(b))
	return math.Acos(min(max(dot, -1), 1))
}

// The launch speed is interpolated from the charge: the minimum when released before a tick
// has elapsed, halfway at half a draw, the full-draw speed at a full draw and no faster for
// holding past it.
func TestTheLaunchSpeedFollowsTheCharge(t *testing.T) {
	t.Parallel()

	h, _ := archer(t, 1)
	full := int(h.sim.fullDrawTicks)
	for name, tc := range map[string]struct {
		held  int
		speed float64
	}{
		"released at once":         {held: 0, speed: ArrowMinDrawSpeed},
		"half a draw":              {held: full / 2, speed: (ArrowMinDrawSpeed + ArrowFullDrawSpeed) / 2},
		"a full draw":              {held: full, speed: ArrowFullDrawSpeed},
		"held three times as long": {held: 3 * full, speed: ArrowFullDrawSpeed},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player := archer(t, 1)
			arrow := looseAfter(t, h, player, tc.held)
			if got := vectorLength(arrow.vel); math.Abs(got-tc.speed) > 1e-9 {
				t.Errorf("launch speed after %d held ticks = %v, want %v", tc.held, got, tc.speed)
			}
		})
	}
}

// A full draw carries no spread at all: the arrow leaves along the look direction exactly.
func TestAFullDrawFliesExactlyAlongTheAim(t *testing.T) {
	t.Parallel()

	for _, look := range [][2]float64{{0, 0}, {0.7, 0.3}, {-2.1, -0.9}, {math.Pi, 1.4}} {
		h, player := archer(t, 1)
		h.aimAt(player, look[0], look[1])
		arrow := looseAfter(t, h, player, int(h.sim.fullDrawTicks))
		h.sim.mu.Lock()
		aim := lookDirection(player.current.yaw, player.current.pitch)
		h.sim.mu.Unlock()
		speed := vectorLength(arrow.vel)
		for axis := range 3 {
			if got := arrow.vel[axis] / speed; math.Abs(got-aim[axis]) > 1e-12 {
				t.Errorf("look %v: direction[%d] = %v, want %v", look, axis, got, aim[axis])
			}
		}
	}
}

// Below a full draw the arrow leaves inside a cone around the aim, whose half-angle narrows
// linearly with the charge. Across many seeds and aims — straight up and straight down
// included, where the basis has to turn — every direction is a unit vector inside its cone,
// and the cone is actually used rather than collapsed onto the aim.
func TestAnUnchargedArrowStaysInsideItsCone(t *testing.T) {
	t.Parallel()

	for _, charge := range []float64{0, 0.5, 0.9} {
		_, spread := arrowLaunch(charge)
		want := ArrowMinDrawSpreadDegrees * math.Pi / 180 * (1 - charge)
		if math.Abs(spread-want) > 1e-15 {
			t.Fatalf("spread at charge %v = %v, want %v", charge, spread, want)
		}
		widest := 0.0
		for seed := range uint64(40) {
			rng := rand.New(rand.NewPCG(seed, bowDrawStream))
			for _, aim := range [][3]float64{
				lookDirection(0, 0), lookDirection(1.3, -0.4), {0, 1, 0}, {0, -1, 0}, lookDirection(-2.8, 0.95),
			} {
				for range 50 {
					direction := spreadDirection(aim, spread, rng)
					if length := vectorLength(direction); math.Abs(length-1) > 1e-9 {
						t.Fatalf("charge %v: direction %v has length %v", charge, direction, length)
					}
					angle := angleBetween(aim, direction)
					if angle > spread+1e-9 {
						t.Fatalf("charge %v: direction %v is %v rad off the aim, beyond the %v cone", charge, direction, angle, spread)
					}
					widest = max(widest, angle)
				}
			}
		}
		if widest < 0.9*spread {
			t.Errorf("charge %v: the widest of 10,000 arrows was %v rad inside a %v cone; the spread is not being drawn", charge, widest, spread)
		}
	}
	if got := spreadDirection([3]float64{0, 0, -1}, 0, nil); got != [3]float64{0, 0, -1} {
		t.Errorf("a zero spread turned the aim into %v", got)
	}
}

// Through the simulation, over several worlds: an arrow released at no charge leaves inside
// the minimum-charge cone, drawn from the world's own generator.
func TestAReleaseAtNoChargeLeavesInsideTheWidestCone(t *testing.T) {
	t.Parallel()

	for seed := range int64(8) {
		h := newVitalsHarnessOver(t, DefaultTickRate, dropTerrain{groundTop: 63}, 8, seed+1)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		player.inventory.mu.Lock()
		player.inventory.slots[equipmentMainHand] = stackOf(ItemBow, 1)
		player.inventory.slots[1] = stackOf(ItemArrow, 1)
		player.inventory.mu.Unlock()
		h.aimAt(player, 0.4, 0.1)

		arrow := looseAfter(t, h, player, 0)
		h.sim.mu.Lock()
		aim := lookDirection(player.current.yaw, player.current.pitch)
		h.sim.mu.Unlock()
		if angle, cone := angleBetween(aim, arrow.vel), ArrowMinDrawSpreadDegrees*math.Pi/180; angle > cone+1e-9 {
			t.Errorf("world %d: the arrow left %v rad off the aim, beyond the %v cone", seed+1, angle, cone)
		}
	}
}

// Damage does not follow the charge: an arrow loosed at no charge and one loosed at a full
// draw both take ArrowDamage off the draugr they hit.
func TestArrowDamageIsTheSameAtEveryCharge(t *testing.T) {
	t.Parallel()

	for name, full := range map[string]bool{"no charge": false, "a full draw": true} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player := archer(t, 1)
			h.keepNight()
			// Close and a little below the eyes, so even the widest cone and the drop over two
			// blocks stay inside the draugr's body.
			target := h.spawnDraugrAt([3]float32{0.5, 64, -1.5})
			h.aimAt(player, 0, -0.2)
			held := 0
			if full {
				held = int(h.sim.fullDrawTicks)
			}
			if _, err := h.draw(player, true, 1); err != nil {
				t.Fatalf("press: %v", err)
			}
			h.advance(held)
			if _, err := h.draw(player, false, 2); err != nil {
				t.Fatalf("release: %v", err)
			}
			for range 10 {
				h.step()
				if h.mobHealth(target) != draugrRow.maxHealth {
					break
				}
			}
			if got := draugrRow.maxHealth - h.mobHealth(target); got != ArrowDamage {
				t.Errorf("the arrow took %d health, want ArrowDamage %d", got, ArrowDamage)
			}
		})
	}
}

// The launch cap is the full-draw speed: a full draw is accepted by the one flight path and
// nothing faster is.
func TestTheLaunchCapIsTheFullDrawSpeed(t *testing.T) {
	t.Parallel()

	if ProjectileMaxLaunchSpeed != ArrowFullDrawSpeed {
		t.Fatalf("ProjectileMaxLaunchSpeed = %v, want the full-draw speed %v", ProjectileMaxLaunchSpeed, ArrowFullDrawSpeed)
	}
	if ArrowMinDrawSpeed <= 0 || ArrowMinDrawSpeed >= ArrowFullDrawSpeed || OrbSpeed > ProjectileMaxLaunchSpeed {
		t.Fatalf("speeds min %v, full %v, orb %v are not ordered under the cap", ArrowMinDrawSpeed, ArrowFullDrawSpeed, OrbSpeed)
	}
	h := newVitalsHarness(t, DefaultTickRate, projectileTerrain{})
	owner, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if _, ok := h.sim.spawnProjectileLocked(vnet.ProjectileKindArrow, owner, projectileOriginLocked(owner), [3]float64{0, 0, -1}, ArrowFullDrawSpeed); !ok {
		t.Error("the flight path refused a full-draw launch")
	}
}
