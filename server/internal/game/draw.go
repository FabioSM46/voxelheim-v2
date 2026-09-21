package game

import (
	"errors"
	"fmt"
	"math"
	"math/rand/v2"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The bow is drawn, held and loosed, and every one of those is measured here.
//
// The wire carries two edges and an ordering tick — `DrawRequest{active: true}` and
// `DrawRequest{active: false}` — and nothing else: no charge, no hold time, no aim. How far
// the string came back is counted in this simulation's ticks from the tick the press was
// admitted to the tick the release is applied, because a client that reported its own hold
// would be stating the arrow's speed. The client animates the string from
// `PlayerVitals.draw_progress` and never runs a timer of its own.
//
// **Energy is paid when the draw starts, and it is never refunded.** A starved player cannot
// begin a draw at all, and a draw cancelled by a mount, a teleport, a death, a leave, a world
// transfer or a bow that left the main hand keeps what it cost — otherwise drawing and
// cancelling would be a free way to hold a ready shot.

// bowDraw is one admitted draw of the bow in the main hand. Every field is guarded by sim.mu.
type bowDraw struct {
	// held is how many ticks the string has been held back. resolveDrawLocked counts it and
	// stops at the full draw, so a draw held at full charge charges no further and costs
	// nothing further for as long as it is held.
	held uint32

	// loosing is an admitted release waiting for the tick. Once it is set held stops
	// counting: the release was made at this charge, and a tick that has to postpone the
	// launch because the inventory is busy does not charge the string for it.
	loosing bool
}

// Draw records one edge of the bow's draw.
//
// Runs on the session's read goroutine and only admits: a press starts the draw and pays for
// it, a release marks the draw for the tick to loose. The tick is what spends the arrow and
// launches it, against the positions that tick produced, exactly as a swing is judged.
//
// A press is admitted when the player is alive, not leaving, not mounted and not blocking, is
// not already drawing, has no swing waiting, the weapon has recovered, the main hand holds a
// bow that is not worn through, an arrow is carried in the hotbar or pack, and the reserve
// holds AttackEnergyCost. Missing arrows answer NoAmmunition, a short reserve answers
// NotEnoughEnergy and mounting answers ActionForbiddenWhileMounted; every other refusal is
// silence. No refusal spends anything.
//
// A release with no draw to loose is silence too. Both edges share one ordering guard,
// separate from the attack's for the reason mining's is separate from movement's.
func (p *Player) Draw(req protocol.DrawRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonUnknown, err
	}
	if p.haveDrawTick && !newerTick(req.ClientTick, p.lastDrawTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale draw client tick %d; the newest accepted is %d", req.ClientTick, p.lastDrawTick)
	}
	p.haveDrawTick, p.lastDrawTick = true, req.ClientTick

	if !req.Active {
		if p.draw == nil {
			return vnet.RefusalReasonUnknown, errors.New("there is no draw to loose")
		}
		p.draw.loosing = true
		return vnet.RefusalReasonUnknown, nil
	}

	if reason, err := p.mountedActionLocked(); err != nil {
		return reason, err
	}
	if p.blocking {
		return vnet.RefusalReasonUnknown, errors.New("a raised shield cannot draw a bow")
	}
	if p.draw != nil {
		// A repeated press without a release. The draw already running is the one the player
		// is holding; a second would restart its charge or be paid for twice.
		return vnet.RefusalReasonUnknown, errors.New("the bow is already drawn")
	}
	if p.pendingSwing != nil {
		return vnet.RefusalReasonUnknown, errors.New("an attack is already waiting for the tick")
	}
	if p.attackCooldown > 0 {
		return vnet.RefusalReasonUnknown, fmt.Errorf("the weapon is recovering for %d more ticks", p.attackCooldown)
	}

	// A session-goroutine admission check, so waiting for this player's inventory is safe,
	// as it is for Attack. The tick re-reads the hand and the arrows with TryLock before it
	// spends either, because both can change between this press and the release.
	p.inventory.mu.Lock()
	bow := p.mainHandHoldsADrawableBowLocked()
	arrows := bow && p.launcherHasAmmunitionLocked(uint8(equipmentMainHand))
	p.inventory.mu.Unlock()
	if !bow {
		return vnet.RefusalReasonUnknown, errors.New("the main hand holds no bow that can be drawn")
	}
	if !arrows {
		return vnet.RefusalReasonNoAmmunition, errors.New("the bow has no arrow to draw")
	}

	cost := uint32(AttackEnergyCost) * energyScale
	if p.energy < cost {
		return vnet.RefusalReasonNotEnoughEnergy, fmt.Errorf("energy %d thousandths is below the %d a draw costs", p.energy, cost)
	}
	p.energy -= cost

	p.draw = &bowDraw{}
	return vnet.RefusalReasonUnknown, nil
}

// drawnLauncher reports whether a registry row is a launcher that is drawn rather than
// swung: one that shoots arrows. The caller needs no lock.
func drawnLauncher(definition itemDefinition) bool {
	return definition.launches == vnet.ProjectileKindArrow
}

// mainHandHoldsADrawableBowLocked reports whether the main hand holds a drawn launcher that
// is not worn through. The caller holds inventory.mu.
func (p *Player) mainHandHoldsADrawableBowLocked() bool {
	stack, held := p.inventory.stackAtLocked(uint8(equipmentMainHand))
	if !held {
		return false
	}
	definition, registered := itemByID(stack.item)
	return registered && drawnLauncher(definition) && (!stack.durable() || stack.durability > 0)
}

// resolveDrawLocked advances this player's draw by one tick, or looses it.
//
// Called from Step straight after resolveAttackLocked, so a loosed arrow is launched from the
// positions this tick produced and lands before any mob acts, as a swing does, and the
// cooldown it starts is counted from the next tick exactly as a swing's is.
//
// A draw that is still held charges by one tick, up to the full draw. A release is resolved
// at the charge the draw had reached, so a press and a release that both arrive before a tick
// has elapsed loose at minimum charge. The caller holds Sim.mu.
func (p *Player) resolveDrawLocked() {
	draw := p.draw
	if draw == nil {
		return
	}
	if !p.alive() {
		// Every death cancels the draw already; this is the invariant, not a live case.
		p.cancelDrawLocked()
		return
	}
	if !draw.loosing {
		if draw.held < p.sim.fullDrawTicks {
			draw.held++
		}
		return
	}

	launched, sampled := p.looseArrowLocked()
	if !sampled {
		// A session goroutine holds the inventory, or a boss reward has frozen it. The tick
		// never waits; the release is kept, at the charge it was made, for the next tick.
		return
	}
	p.draw = nil
	if !launched {
		// The last arrow left the pack between the press and the release. Nothing is fired,
		// and the energy the press paid stays spent.
		return
	}

	// The cooldown starts at the release, and the launch goes through the one flight path:
	// this function only chooses the speed and the direction.
	speed, spread := arrowLaunch(draw.charge(p.sim.fullDrawTicks))
	p.attackCooldown = p.sim.bowCooldownTicks
	aim := lookDirection(p.current.yaw, p.current.pitch)
	p.sim.spawnProjectileLocked(
		vnet.ProjectileKindArrow,
		p,
		projectileOriginLocked(p),
		spreadDirection(aim, spread, p.sim.draws),
		speed,
	)
}

// charge is how far the string came back: 0 at the press, 1 at a full draw and never more,
// counted in ticks only.
func (d *bowDraw) charge(fullDrawTicks uint32) float64 {
	full := max(fullDrawTicks, 1)
	return float64(min(d.held, full)) / float64(full)
}

// arrowLaunch is the launch speed and the spread half-angle, in radians, of an arrow loosed
// at a charge. Both are linear in the charge: the speed from ArrowMinDrawSpeed to
// ArrowFullDrawSpeed, and the cone from ArrowMinDrawSpreadDegrees to exactly zero. The
// charge says nothing about damage.
func arrowLaunch(charge float64) (speed, spread float64) {
	charge = min(max(charge, 0), 1)
	speed = ArrowMinDrawSpeed + (ArrowFullDrawSpeed-ArrowMinDrawSpeed)*charge
	spread = ArrowMinDrawSpreadDegrees * math.Pi / 180 * (1 - charge)
	return speed, spread
}

// spreadDirection is a unit direction drawn uniformly over the spherical cap of half-angle
// spread around aim, which must be a unit vector. A zero spread returns aim unchanged and
// draws nothing, so a full draw flies exactly where the player looked.
//
// The generator is the simulation's own, guarded by Sim.mu and advanced only on the tick,
// on the terms the spawn and loot generators are: a client can neither see nor steer it.
func spreadDirection(aim [3]float64, spread float64, rng *rand.Rand) [3]float64 {
	if spread <= 0 {
		return aim
	}
	// Uniform over the cap: cos(theta) uniform on [cos(spread), 1], the azimuth uniform.
	cosTheta := 1 - rng.Float64()*(1-math.Cos(spread))
	sinTheta := math.Sqrt(max(1-cosTheta*cosTheta, 0))
	sinPhi, cosPhi := math.Sincos(rng.Float64() * 2 * math.Pi)

	// Any axis not parallel to aim builds the basis perpendicular to it.
	helper := [3]float64{0, 1, 0}
	if math.Abs(aim[1]) > 0.9 {
		helper = [3]float64{1, 0, 0}
	}
	u := cross(helper, aim)
	length := vectorLength(u)
	u = [3]float64{u[0] / length, u[1] / length, u[2] / length}
	v := cross(aim, u)

	var direction [3]float64
	for axis := range 3 {
		direction[axis] = aim[axis]*cosTheta + (u[axis]*cosPhi+v[axis]*sinPhi)*sinTheta
	}
	return direction
}

func cross(a, b [3]float64) [3]float64 {
	return [3]float64{
		a[1]*b[2] - a[2]*b[1],
		a[2]*b[0] - a[0]*b[2],
		a[0]*b[1] - a[1]*b[0],
	}
}

// bowDrawStream is the second word of the draw generator's PCG seed, so a bow's spread never
// draws the spawn director's or the loot table's numbers, and neither of them draws its.
const bowDrawStream = 0x766F78656C626F77 // "voxelbow"

func newDrawRNG(worldSeed int64) *rand.Rand {
	return rand.New(rand.NewPCG(uint64(worldSeed), bowDrawStream))
}

// slotHoldsADrawnLauncherLocked reports whether a slot holds a bow, whatever its condition.
// The caller holds inventory.mu.
func (p *Player) slotHoldsADrawnLauncherLocked(slot uint8) bool {
	stack, held := p.inventory.stackAtLocked(slot)
	if !held {
		return false
	}
	definition, registered := itemByID(stack.item)
	return registered && drawnLauncher(definition)
}

// looseArrowLocked spends one arrow and one point of the main-hand bow's durability, and
// reports whether it did and whether the inventory could be read at all. The first answer is
// false when the hand no longer holds a usable bow or no arrow is left; the second is false
// when the tick must postpone. The caller holds Sim.mu.
func (p *Player) looseArrowLocked() (launched, sampled bool) {
	if !p.inventory.mu.TryLock() {
		return false, false
	}
	defer p.inventory.mu.Unlock()

	if !p.mainHandHoldsADrawableBowLocked() {
		return false, true
	}
	stack, _ := p.inventory.stackAtLocked(uint8(equipmentMainHand))
	definition, _ := itemByID(stack.item)
	return p.spendLaunchLocked(uint8(equipmentMainHand), definition)
}

// cancelDrawLocked ends a draw without loosing it: nothing is fired, no arrow is spent and
// the energy the press paid is not refunded.
//
// It sits beside lowerShieldLocked at every authoritative removal — teleport, mounting,
// leaving, death and world transfer — and is also reached when the main-hand bow is moved,
// emptied or worn out (refreshWornLocked, MoveInventory) and when a shield rises
// (settleShieldLocked). The caller holds sim.mu.
func (p *Player) cancelDrawLocked() {
	p.draw = nil
}

// drawProgressLocked is the wire's draw_progress: zero when not drawing, and while drawing
// the charge as a fraction of 255, never below 1 so a draw admitted this tick is still told
// apart from no draw at all. 255 is a full draw still held. The caller holds sim.mu.
func (p *Player) drawProgressLocked() uint8 {
	if p.draw == nil {
		return 0
	}
	full := max(p.sim.fullDrawTicks, 1)
	held := min(p.draw.held, full)
	return uint8(max(uint64(held)*255/uint64(full), 1))
}
