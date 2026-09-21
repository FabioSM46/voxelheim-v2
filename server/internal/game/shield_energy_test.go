package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// parryCost is ParryEnergyCost in the thousandths the reserve is stored in.
const parryCost = uint32(ParryEnergyCost) * energyScale

// A swing made with the shield up is dropped before it spends anything, so it can neither
// leave the reserve below the parry's cost under a raised shield nor pay for a swing the
// per-tick settle then discards. The shield stays up across the tick that follows.
func TestASwingWithTheShieldUpSpendsNothingAndKeepsTheGuard(t *testing.T) {
	t.Parallel()

	h, player, out := shieldInHand(t)
	setEnergy(h, player, parryCost)
	if reason := player.Block(true); reason != vnet.RefusalReasonUnknown {
		t.Fatalf("a press at exactly the parry's cost answered %s", reason)
	}
	for tick := uint32(1); tick <= 3; tick++ {
		reason, err := player.Attack(protocol.AttackRequest{Slot: mainHandSlot, ClientTick: tick})
		if err != nil || reason != vnet.RefusalReasonUnknown {
			t.Fatalf("a swing with the shield up answered reason %s, error %v; it is dropped in silence", reason, err)
		}
		h.sim.mu.Lock()
		energy, pending := player.energy, player.pendingSwing
		h.sim.mu.Unlock()
		if energy < parryCost || pending != nil {
			t.Fatalf("swing %d with the shield up left %d thousandths and a pending swing %v", tick, energy, pending != nil)
		}
		h.step()
		if !shieldAgrees(t, h, player, out) {
			t.Fatalf("tick %d: a swing with the shield up lowered it", h.tick)
		}
	}
}

// A generous bound on how long any of these scenarios waits for a regeneration: the full
// reserve refills in eight seconds, so twelve is never the thing that ends a loop.
var regenerationTicks = 12 * int(DefaultTickRate)

// shieldInHand is a player holding a usable shield with nothing pressed yet, and developer
// commands enabled so a teleport is reachable.
func shieldInHand(t *testing.T) (*vitalsHarness, *Player, *dropSink) {
	t.Helper()
	h, player, out := commandPlayer(t, true)
	equipShield(t, player, WoodenShieldMaxDurability)
	return h, player, out
}

func setEnergy(h *vitalsHarness, player *Player, thousandths uint32) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	player.energy = thousandths
}

func energyOf(h *vitalsHarness, player *Player) uint32 {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return player.energy
}

// shieldAgrees fails the test unless the simulation's flag, the recipient's own vitals and
// the blocking vector of the newest snapshot all give the same answer, and returns it.
func shieldAgrees(t *testing.T, h *vitalsHarness, player *Player, out *dropSink) bool {
	t.Helper()
	snapshot := newestSnapshot(t, out)
	self := snapshot.SelfVitals(nil).Blocking()
	listed := false
	for i := range snapshot.BlockingPlayersLength() {
		if snapshot.BlockingPlayers(i) == player.entityID {
			listed = true
		}
	}
	h.sim.mu.Lock()
	blocking := player.blocking
	h.sim.mu.Unlock()
	if self != listed || self != blocking {
		t.Fatalf("tick %d: vitals blocking %v, snapshot lists the player %v, simulation %v", h.tick, self, listed, blocking)
	}
	return blocking
}

// A press is refused below the parry's cost, answered once per press rather than once per
// repeated request or per tick, and admitted at exactly the cost.
func TestABlockPressNeedsTheParryCostAndIsRefusedOncePerPress(t *testing.T) {
	t.Parallel()

	h, player, out := shieldInHand(t)
	setEnergy(h, player, 24*energyScale)
	if reason := player.Block(true); reason != vnet.RefusalReasonNotEnoughEnergy {
		t.Fatalf("a press at 24 energy answered %s, want NotEnoughEnergy", reason)
	}
	if h.vitals(player).Blocking {
		t.Fatal("a press at 24 energy raised the shield")
	}
	if reason := player.Block(true); reason != vnet.RefusalReasonUnknown {
		t.Errorf("a repeated press with no release answered %s again; a press is answered once", reason)
	}
	// The tick re-derives the shield and has nowhere to put a refusal: a tick short of
	// the cost changes nothing and repeats nothing.
	setEnergy(h, player, 24*energyScale)
	h.step()
	if shieldAgrees(t, h, player, out) {
		t.Fatal("a tick still short of the cost raised the shield")
	}

	player.Block(false)
	if reason := player.Block(true); reason != vnet.RefusalReasonNotEnoughEnergy {
		t.Errorf("a new press after a release answered %s, want NotEnoughEnergy", reason)
	}

	player.Block(false)
	setEnergy(h, player, parryCost)
	if reason := player.Block(true); reason != vnet.RefusalReasonUnknown {
		t.Errorf("a press at exactly 25 energy answered %s", reason)
	}
	if !h.vitals(player).Blocking {
		t.Fatal("a press at exactly 25 energy left the shield down")
	}
}

// A press refused for anything but energy is silence and leaves no intent behind.
func TestABlockPressWithNoShieldIsSilentAndNotHeld(t *testing.T) {
	t.Parallel()

	h, player, out := commandPlayer(t, true)
	setEnergy(h, player, 0)
	if reason := player.Block(true); reason != vnet.RefusalReasonUnknown {
		t.Errorf("a press with an empty off hand answered %s; it is silence", reason)
	}
	equipShield(t, player, WoodenShieldMaxDurability)
	for range regenerationTicks {
		h.step()
		if shieldAgrees(t, h, player, out) {
			t.Fatalf("tick %d: a press made with no shield raised one equipped afterwards", h.tick)
		}
	}
}

// Held at an empty reserve, the shield rises with no second press on the first tick the
// refill reaches the parry's cost — and not a tick earlier.
func TestAHeldPressRaisesTheShieldOnTheTickTheReserveReachesTheCost(t *testing.T) {
	t.Parallel()

	h, player, out := shieldInHand(t)
	setEnergy(h, player, 0)
	if reason := player.Block(true); reason != vnet.RefusalReasonNotEnoughEnergy {
		t.Fatalf("a press at 0 energy answered %s, want NotEnoughEnergy", reason)
	}
	for range regenerationTicks {
		h.step()
		blocking := shieldAgrees(t, h, player, out)
		energy := energyOf(h, player)
		if energy < parryCost {
			if blocking {
				t.Fatalf("tick %d: the shield rose at %d thousandths, short of %d", h.tick, energy, parryCost)
			}
			continue
		}
		if !blocking {
			t.Fatalf("tick %d: the reserve reached %d thousandths and the held shield stayed down", h.tick, energy)
		}
		return
	}
	t.Fatal("the reserve never reached the parry's cost")
}

// A parry absorbed from 30 leaves 5: the shield lowers on that tick, and rises again once
// the refill has brought the reserve back while the press is still held.
func TestAParryThatSpendsTheReserveLowersTheShieldUntilItRegenerates(t *testing.T) {
	t.Parallel()

	h, player, out := shieldInHand(t)
	if reason := player.Block(true); reason != vnet.RefusalReasonUnknown {
		t.Fatalf("a press at full energy answered %s", reason)
	}
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	equipLeatherSet(t, player)
	h.aimAt(player, 0, 0)
	armMobBlow(t, h, mobID, player)

	// Step refills before the mob swings, so store one refill short of 30.
	setEnergy(h, player, 30*energyScale-h.sim.energyRefill)
	before := h.vitals(player).Health
	h.step()
	if got := before - h.vitals(player).Health; got != 4 {
		t.Fatalf("the parry cost %d health, want the halved 4", got)
	}
	if got := energyOf(h, player); got != 5*energyScale {
		t.Fatalf("energy after parrying from 30 = %d, want %d", got, 5*energyScale)
	}
	if shieldAgrees(t, h, player, out) {
		t.Fatal("the shield stayed raised on the tick the parry left 5 energy")
	}

	// Nothing else swings while the reserve refills: this is about the refill, not a
	// second blow.
	h.sim.mu.Lock()
	delete(h.sim.mobs, mobID)
	h.sim.mu.Unlock()
	for range regenerationTicks {
		h.step()
		blocking := shieldAgrees(t, h, player, out)
		if energyOf(h, player) < parryCost {
			if blocking {
				t.Fatalf("tick %d: the shield rose short of the parry's cost", h.tick)
			}
			continue
		}
		if !blocking {
			t.Fatalf("tick %d: the reserve came back and the held shield did not", h.tick)
		}
		return
	}
	t.Fatal("the reserve never came back to the parry's cost")
}

// A release forgets the press, so a refill that would have raised a held shield raises
// nothing — and a fresh press at a full reserve raises it at once.
func TestAReleaseForgetsThePress(t *testing.T) {
	t.Parallel()

	h, player, out := shieldInHand(t)
	setEnergy(h, player, 0)
	player.Block(true)
	if reason := player.Block(false); reason != vnet.RefusalReasonUnknown {
		t.Errorf("a release answered %s", reason)
	}
	for range regenerationTicks {
		h.step()
		if shieldAgrees(t, h, player, out) {
			t.Fatalf("tick %d: a released press raised the shield", h.tick)
		}
	}
	if reason := player.Block(true); reason != vnet.RefusalReasonUnknown || !h.vitals(player).Blocking {
		t.Errorf("a new press at a full reserve answered %s and left blocking %v", reason, h.vitals(player).Blocking)
	}
}

// Every authoritative removal of the shield forgets the press too, so none of them is
// undone by the refill raising the shield again afterwards.
func TestEveryAuthoritativeRemovalForgetsTheHeldPress(t *testing.T) {
	t.Parallel()

	for name, remove := range map[string]func(t *testing.T, h *vitalsHarness, player *Player){
		"teleport": func(t *testing.T, _ *vitalsHarness, player *Player) {
			if _, err := player.Chat("/teleport 0 64 0"); err != nil {
				t.Fatalf("teleport: %v", err)
			}
		},
		"mounting": func(t *testing.T, h *vitalsHarness, player *Player) {
			prepareMount(player, vnet.MountKindGreyHorse, true)
			if reason, err := player.Mount(vnet.MountKindGreyHorse); err != nil || reason != vnet.RefusalReasonUnknown {
				t.Fatalf("Mount: reason %s, error %v", reason, err)
			}
			h.advance(int(h.sim.castTicks))
			if got := mountedKind(player); got != vnet.MountKindGreyHorse {
				t.Fatalf("mounted = %s, want GreyHorse", got)
			}
			player.Dismount()
			// The cast took long enough to refill the reserve; empty it again so the
			// refill below is the one that would raise a shield still held.
			setEnergy(h, player, 0)
		},
		"death": func(t *testing.T, h *vitalsHarness, player *Player) {
			h.hurt(player, PlayerMaxHealth)
			for range regenerationTicks {
				if h.vitals(player).LifeState == vnet.LifeStateAlive {
					break
				}
				h.step()
			}
			if h.vitals(player).LifeState != vnet.LifeStateAlive {
				t.Fatal("the player never respawned")
			}
		},
		"unequipped": func(t *testing.T, _ *vitalsHarness, player *Player) {
			for _, move := range []protocol.InventoryMoveRequest{
				{From: uint8(equipmentOffHand), To: 4, Count: 1},
				{From: 4, To: uint8(equipmentOffHand), Count: 1},
			} {
				if _, err := player.MoveInventory(move); err != nil {
					t.Fatalf("move shield %d -> %d: %v", move.From, move.To, err)
				}
			}
		},
		"leaving": func(_ *testing.T, _ *vitalsHarness, player *Player) {
			player.BeginLeaving()
			player.CancelLeaving()
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h, player, out := shieldInHand(t)
			setEnergy(h, player, 0)
			if reason := player.Block(true); reason != vnet.RefusalReasonNotEnoughEnergy {
				t.Fatalf("a press at 0 energy answered %s, want NotEnoughEnergy", reason)
			}
			remove(t, h, player)
			for range regenerationTicks {
				h.step()
				if shieldAgrees(t, h, player, out) {
					t.Fatalf("tick %d: the shield rose after %s cleared the press", h.tick, name)
				}
			}
		})
	}
}
