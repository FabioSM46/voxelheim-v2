package game

import "testing"

// fullEnergy is a full reserve in the stored thousandths.
const fullEnergy = uint32(MaxEnergy) * energyScale

// The refill is derived from the rate once, and it is exact where it matters: 625
// thousandths a tick at the default rate is 12.5 points a second.
func TestEnergyRefillIsDerivedFromTheTickRate(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		rate uint8
		want uint32
	}{
		{1, 12500},
		{DefaultTickRate, 625},
		{50, 250},
		// 12,500 / 255 is 49.02: rounded down, never away.
		{255, 49},
	} {
		if got := energyRegenPerTick(tc.rate); got != tc.want {
			t.Errorf("energyRegenPerTick(%d) = %d, want %d", tc.rate, got, tc.want)
		}
	}
}

// Zero to full in eight seconds at 20 Hz, to the tick, and no further.
func TestEnergyRefillsFromEmptyInEightSecondsAndStopsAtTheCap(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if player.energy != fullEnergy {
		t.Fatalf("a joining player holds %d energy, want a full %d", player.energy, fullEnergy)
	}

	player.energy = 0
	ticks := 8 * int(DefaultTickRate)
	for range ticks - 1 {
		player.advanceVitalsLocked()
	}
	if player.energy >= fullEnergy {
		t.Fatalf("one tick before eight seconds energy is already %d", player.energy)
	}
	player.advanceVitalsLocked()
	if player.energy != fullEnergy {
		t.Fatalf("after eight seconds energy = %d, want %d", player.energy, fullEnergy)
	}
	player.advanceVitalsLocked()
	if player.energy != fullEnergy {
		t.Errorf("a full reserve refilled past the cap to %d", player.energy)
	}
}

// Nothing a fight does pauses the refill: not a raised shield, not a swing waiting for
// the tick, not a landed hit, and not the leave linger either.
func TestEnergyRefillNeverPauses(t *testing.T) {
	t.Parallel()

	for name, arrange := range map[string]func(*Player){
		"idle":                  func(*Player) {},
		"blocking":              func(p *Player) { p.blocking = true },
		"swing pending":         func(p *Player) { p.pendingSwing = &pendingSwing{slot: 0} },
		"weapon recovering":     func(p *Player) { p.attackCooldown = 5 },
		"just hit":              func(p *Player) { p.damageLocked(1) },
		"leaving":               func(p *Player) { p.leaving = true },
		"starving and hurt":     func(p *Player) { p.hunger, p.health = 0, 1 },
		"inside the protection": func(p *Player) { p.protectionTicks = 10 },
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			arrange(player)
			player.energy = 0
			player.advanceVitalsLocked()
			if player.energy != h.sim.energyRefill {
				t.Errorf("energy after one tick = %d, want %d", player.energy, h.sim.energyRefill)
			}
		})
	}
}

// A new life starts full, however little the last one left.
func TestRespawnRestoresAFullEnergyReserve(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	player.dieLocked()
	player.energy = 0
	player.respawnLocked()
	if player.energy != fullEnergy {
		t.Errorf("energy after respawn = %d, want %d", player.energy, fullEnergy)
	}
}

// The wire carries whole points rounded down, so a reserve a fraction short of a swing
// is never shown as enough for one; the maximum is always the non-zero denominator.
func TestVitalsCarryEnergyRoundedDown(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	for _, tc := range []struct {
		stored uint32
		want   uint16
	}{
		{0, 0},
		{999, 0},
		{uint32(AttackEnergyCost)*energyScale - 1, AttackEnergyCost - 1},
		{uint32(AttackEnergyCost) * energyScale, AttackEnergyCost},
		{fullEnergy, MaxEnergy},
	} {
		h.sim.mu.Lock()
		player.energy = tc.stored
		h.sim.mu.Unlock()

		vitals := h.vitals(player)
		if vitals.Energy != tc.want || vitals.MaxEnergy != MaxEnergy {
			t.Errorf("stored %d thousandths sent as %d/%d, want %d/%d",
				tc.stored, vitals.Energy, vitals.MaxEnergy, tc.want, MaxEnergy)
		}
	}
}
