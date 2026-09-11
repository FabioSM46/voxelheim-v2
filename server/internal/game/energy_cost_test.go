package game

import (
	"errors"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// An attack costs AttackEnergyCost, checked and spent at admission: 24 points is a
// refusal that queues nothing and spends nothing, 25 is a swing that leaves nothing.
func TestAnAttackIsRefusedAt24EnergyAndAdmittedAt25(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name      string
		stored    uint32
		admitted  bool
		remaining uint32
	}{
		{"24 points", 24 * energyScale, false, 24 * energyScale},
		{"a thousandth short of 25", 25*energyScale - 1, false, 25*energyScale - 1},
		{"25 points", 25 * energyScale, true, 0},
		{"full", fullEnergy, true, fullEnergy - 25*energyScale},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.sim.mu.Lock()
			player.energy = tc.stored
			h.sim.mu.Unlock()

			reason, err := player.Attack(protocol.AttackRequest{Slot: 0, ClientTick: 1})

			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			if tc.admitted {
				if err != nil || reason != vnet.RefusalReasonUnknown {
					t.Fatalf("Attack = %s, %v; want admitted", reason, err)
				}
				if player.pendingSwing == nil {
					t.Error("an admitted swing queued nothing")
				}
			} else {
				if err == nil || reason != vnet.RefusalReasonNotEnoughEnergy {
					t.Fatalf("Attack = %s, %v; want NotEnoughEnergy", reason, err)
				}
				if player.pendingSwing != nil {
					t.Error("a starved swing was queued")
				}
			}
			if player.energy != tc.remaining {
				t.Errorf("energy after the request = %d, want %d", player.energy, tc.remaining)
			}
		})
	}
}

// Every refusal that is decided before energy leaves the reserve untouched: a request
// that did not become a swing is not charged for one.
func TestAnAttackRefusedForAnotherReasonSpendsNoEnergy(t *testing.T) {
	t.Parallel()

	for name, arrange := range map[string]func(*vitalsHarness, *Player){
		"recovering": func(_ *vitalsHarness, p *Player) { p.attackCooldown = 3 },
		"already waiting": func(_ *vitalsHarness, p *Player) {
			p.pendingSwing = &pendingSwing{slot: 0}
		},
		"shield raised": func(_ *vitalsHarness, p *Player) { p.blocking = true },
		"dead":          func(_ *vitalsHarness, p *Player) { p.dieLocked() },
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.sim.mu.Lock()
			arrange(h, player)
			h.sim.mu.Unlock()

			_, _ = player.Attack(protocol.AttackRequest{Slot: 0, ClientTick: 1})

			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			if player.energy != fullEnergy {
				t.Errorf("energy after a refused request = %d, want the untouched %d", player.energy, fullEnergy)
			}
		})
	}
}

// A full reserve is four swings and not a fifth. Every other admission guard is cleared
// between requests, so energy is the only thing left to answer the fifth.
func TestAFullReserveIsFourSwingsAndNotAFifth(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	for tick := uint32(1); tick <= 5; tick++ {
		reason, err := player.Attack(protocol.AttackRequest{Slot: 0, ClientTick: tick})
		if tick <= 4 && err != nil {
			t.Fatalf("swing %d from a full reserve was refused: %s, %v", tick, reason, err)
		}
		if tick == 5 && (err == nil || reason != vnet.RefusalReasonNotEnoughEnergy) {
			t.Fatalf("a fifth swing = %s, %v; want NotEnoughEnergy", reason, err)
		}
		h.sim.mu.Lock()
		player.pendingSwing, player.attackCooldown = nil, 0
		h.sim.mu.Unlock()
	}
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if player.energy != 0 {
		t.Errorf("four swings left %d thousandths, want 0", player.energy)
	}
}

// A shield that absorbs a blow spends ParryEnergyCost from the reserve, exactly once.
func TestAnAbsorbedBlowSpendsParryEnergy(t *testing.T) {
	t.Parallel()

	h, player, _ := raisedShield(t)
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	equipLeatherSet(t, player)
	h.aimAt(player, 0, 0)
	armMobBlow(t, h, mobID, player)

	h.sim.mu.Lock()
	player.energy = uint32(ParryEnergyCost) * energyScale
	h.sim.mu.Unlock()
	before := h.vitals(player).Health
	h.step()

	if got := before - h.vitals(player).Health; got != 4 {
		t.Errorf("an affordable parry cost %d health, want the halved 4", got)
	}
	h.sim.mu.Lock()
	// Step refills before the mob swings, so the parry spends 25 points from 25 plus one
	// refill and leaves exactly that refill behind.
	if got, want := player.energy, h.sim.energyRefill; got != want {
		t.Errorf("energy after the parry = %d, want %d (one refill, then 25 spent)", got, want)
	}
	h.sim.mu.Unlock()
	player.inventory.mu.Lock()
	defer player.inventory.mu.Unlock()
	if got := player.inventory.slots[equipmentOffHand].durability; got != WoodenShieldMaxDurability-1 {
		t.Errorf("shield durability = %d, want %d", got, WoodenShieldMaxDurability-1)
	}
}

// A guard with less than the parry's cost absorbs nothing: full damage, no durability,
// no threat, and the reserve keeps what it had.
func TestAStarvedGuardTakesTheFullBlowAndSpendsNothing(t *testing.T) {
	t.Parallel()

	h, player, _ := raisedShield(t)
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	equipLeatherSet(t, player)
	h.aimAt(player, 0, 0)
	armMobBlow(t, h, mobID, player)

	// Step refills before any mob swings, so the reserve must still be short of the cost
	// after that one refill: a thousandth below what the refill would complete.
	stored := uint32(ParryEnergyCost)*energyScale - h.sim.energyRefill - 1
	h.sim.mu.Lock()
	player.energy = stored
	h.sim.mu.Unlock()
	before := h.vitals(player).Health
	h.step()

	if got := before - h.vitals(player).Health; got != 8 {
		t.Errorf("a starved guard took %d damage, want the unblocked 8", got)
	}
	h.sim.mu.Lock()
	if got, want := player.energy, stored+h.sim.energyRefill; got != want {
		t.Errorf("energy after a starved guard = %d, want %d (nothing spent, one refill)", got, want)
	}
	stillBlocking := player.blocking
	h.sim.mu.Unlock()
	if !stillBlocking {
		t.Error("a starved guard lowered the shield; only the absorption is refused")
	}
	player.inventory.mu.Lock()
	durability := player.inventory.slots[equipmentOffHand].durability
	player.inventory.mu.Unlock()
	if durability != WoodenShieldMaxDurability {
		t.Errorf("a starved guard spent shield durability: %d", durability)
	}
	if got := threatFor(t, h, mobID, player.entityID); got == ShieldTauntThreat {
		t.Errorf("a starved guard earned the block taunt %v", got)
	}
}

// Holding the shield up costs nothing, and a blow from behind is not a parry either.
func TestHoldingTheShieldAndARearBlowCostNoEnergy(t *testing.T) {
	t.Parallel()

	t.Run("held with nothing to block", func(t *testing.T) {
		t.Parallel()
		h, player, _ := raisedShield(t)
		for range 3 * int(DefaultTickRate) {
			h.step()
		}
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		if player.energy != fullEnergy || !player.blocking {
			t.Errorf("after three seconds of holding: energy %d, blocking %v; want %d and true", player.energy, player.blocking, fullEnergy)
		}
	})

	t.Run("a blow from behind", func(t *testing.T) {
		t.Parallel()
		h, player, _ := raisedShield(t)
		mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
		h.aimAt(player, 3.141592653589793, 0)
		armMobBlow(t, h, mobID, player)
		h.step()
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		if player.energy != fullEnergy {
			t.Errorf("a rear blow spent energy: %d", player.energy)
		}
	})
}

// The sentinel stays the one ErrActionForbiddenWhileMounted names; energy is never the
// reason a mounted swing is refused, and a mounted refusal spends none.
func TestAMountedSwingIsRefusedBeforeEnergyIsAsked(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.sim.mu.Lock()
	player.mounted = vnet.MountKindBrownHorse
	player.energy = 0
	h.sim.mu.Unlock()

	reason, err := player.Attack(protocol.AttackRequest{Slot: 0, ClientTick: 1})
	if !errors.Is(err, ErrActionForbiddenWhileMounted) || reason != vnet.RefusalReasonActionForbiddenWhileMounted {
		t.Fatalf("mounted starved Attack = %s, %v; want the mounted refusal", reason, err)
	}
}
